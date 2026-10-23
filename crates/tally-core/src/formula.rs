//! Column formulas: one expression computes a column for every row; a column
//! reference means that column's value on the same row.
//!
//! ```text
//! coalesce(to(price_uah, USD), price_usd)   unit cost in USD from whichever price is filled in
//! unit_usd * qty                             line total
//! to(c8, EUR)                                columns by their ID work too
//! [Price (UAH)] * 1.2                        or by display name, in brackets
//! ```
//!
//! A column can be referred to by its ID (`c4`), its variable name (`price_uah`) or
//! its display name in brackets (`[Price (UAH)]`). Three letters that are not a
//! column are a currency (`USD`, `usd`); in `to(x, usd)` they always are. Formulas are stored with column IDs, so renaming a column never
//! breaks one. Amounts keep their currency: adding UAH to USD is an error, never a
//! silent mix; `to()` converts.

use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;

use crate::money::Currency;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(Decimal),
    /// A bare name as typed: a column ID, a variable name or a currency.
    Name(String),
    /// A display name, typed in brackets.
    Display(String),
    /// A resolved column reference (column ID).
    Col(String),
    /// A resolved currency code.
    Cur(Currency),
    /// A parameter of a user function (by position).
    Param(usize),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// A value in a cell or produced by a formula.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Number(Decimal),
    Money(Decimal, Currency),
    Text(String),
    Currency(Currency),
    Error(String),
}

impl Value {
    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }
}

/// What a formula can see while it is evaluated for one row.
pub trait Context {
    /// The value of a column (by ID) in the current row, or `None` if there is no such column.
    fn column(&self, id: &str) -> Option<Value>;
    /// Convert an amount between currencies at the sheet's rates.
    fn convert(&self, amount: Decimal, from: &Currency, to: &Currency) -> Result<Decimal, String>;
    /// User-defined functions available to formulas.
    fn functions(&self) -> Option<&Functions> {
        None
    }
    /// A user function's parameter values, while its body is evaluated.
    fn param(&self, _i: usize) -> Option<Value> {
        None
    }
    /// How deep in user-function calls this evaluation is.
    fn depth(&self) -> usize {
        0
    }
}

// ------------------------------------------------------------------ user functions

/// A function defined by the user, e.g. `vat(x) = x * 0.2`, usable in every formula.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserFunction {
    pub name: String,
    pub params: Vec<String>,
    /// The formula, written with the parameter names.
    pub body: String,
    #[serde(default)]
    pub description: String,
}

/// User functions, compiled and ready to call.
#[derive(Clone, Debug, Default)]
pub struct Functions {
    map: std::collections::HashMap<String, (UserFunction, Expr)>,
}

const MAX_DEPTH: usize = 32;

fn check_ident(kind: &str, n: &str) -> Result<(), String> {
    let ok = n.starts_with(|c: char| c.is_ascii_lowercase()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!("{kind} {n:?}: use lowercase letters, digits and _, starting with a letter"))
    }
}

/// Compile a function body: names must be its parameters (or currencies).
pub fn compile_function(f: &UserFunction) -> Result<Expr, String> {
    check_ident("function name", &f.name)?;
    if FUNCTIONS.contains(&f.name.as_str()) {
        return Err(format!("{} is a built-in function", f.name));
    }
    for (i, p) in f.params.iter().enumerate() {
        check_ident("parameter", p)?;
        if f.params[..i].contains(p) {
            return Err(format!("parameter {p} is listed twice"));
        }
    }
    fn walk(e: &Expr, params: &[String]) -> Result<Expr, String> {
        Ok(match e {
            Expr::Name(n) => match params.iter().position(|p| p == n) {
                Some(i) => Expr::Param(i),
                None if is_currency_code(n) => Expr::Cur(Currency::new(n).map_err(|e| e.to_string())?),
                None => return Err(format!("{n} is not a parameter (a function can only use its parameters)")),
            },
            Expr::Display(n) => return Err(format!("[{n}]: a function cannot refer to columns; pass them in")),
            Expr::Neg(a) => Expr::Neg(Box::new(walk(a, params)?)),
            Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(walk(a, params)?), Box::new(walk(b, params)?)),
            Expr::Call(g, args) => Expr::Call(g.clone(), args.iter().map(|a| walk(a, params)).collect::<Result<_, _>>()?),
            other => other.clone(),
        })
    }
    walk(&parse(&f.body)?, &f.params)
}

impl Functions {
    /// Compile a set of functions. Ones that do not compile are returned as problems.
    pub fn new(list: impl IntoIterator<Item = UserFunction>) -> (Self, Vec<String>) {
        let mut map = std::collections::HashMap::new();
        let mut problems = Vec::new();
        for f in list {
            match compile_function(&f) {
                Ok(e) => {
                    map.insert(f.name.clone(), (f, e));
                }
                Err(e) => problems.push(format!("function {}: {e}", f.name)),
            }
        }
        (Self { map }, problems)
    }

    pub fn get(&self, name: &str) -> Option<&(UserFunction, Expr)> {
        self.map.get(name)
    }

    pub fn list(&self) -> Vec<&UserFunction> {
        let mut v: Vec<_> = self.map.values().map(|(f, _)| f).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Functions this one calls, directly or not, would call it back?
    pub fn has_loop(&self, name: &str) -> bool {
        fn calls(e: &Expr, out: &mut Vec<String>) {
            match e {
                Expr::Call(f, args) => {
                    out.push(f.clone());
                    args.iter().for_each(|a| calls(a, out));
                }
                Expr::Neg(a) => calls(a, out),
                Expr::Bin(_, a, b) => {
                    calls(a, out);
                    calls(b, out);
                }
                _ => {}
            }
        }
        let mut stack = vec![name.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(n) = stack.pop() {
            let Some((_, body)) = self.map.get(&n) else { continue };
            let mut out = Vec::new();
            calls(body, &mut out);
            for c in out {
                if c == name {
                    return true;
                }
                if seen.insert(c.clone()) {
                    stack.push(c);
                }
            }
        }
        false
    }
}

/// Evaluate a formula that uses no columns (e.g. `vat(100)` when trying out a
/// function). Currency conversion is not available here.
pub fn eval_standalone(src: &str, functions: &Functions) -> Value {
    struct Plain<'a>(&'a Functions);
    impl Context for Plain<'_> {
        fn column(&self, _id: &str) -> Option<Value> {
            None
        }
        fn convert(&self, _a: Decimal, from: &Currency, to: &Currency) -> Result<Decimal, String> {
            Err(format!("{from}→{to}: conversions need a sheet's rates; try it in a column"))
        }
        fn functions(&self) -> Option<&Functions> {
            Some(self.0)
        }
    }
    match parse(src).and_then(|e| resolve(&e, &[])) {
        Ok(e) => eval(&e, &Plain(functions)),
        Err(e) => Value::Error(e),
    }
}

/// Evaluating a user function's body: its parameters, and everything else from the caller.
struct Call<'a> {
    args: Vec<Value>,
    parent: &'a dyn Context,
}

impl Context for Call<'_> {
    fn column(&self, _id: &str) -> Option<Value> {
        None
    }
    fn convert(&self, amount: Decimal, from: &Currency, to: &Currency) -> Result<Decimal, String> {
        self.parent.convert(amount, from, to)
    }
    fn functions(&self) -> Option<&Functions> {
        self.parent.functions()
    }
    fn param(&self, i: usize) -> Option<Value> {
        self.args.get(i).cloned()
    }
    fn depth(&self) -> usize {
        self.parent.depth() + 1
    }
}

// ------------------------------------------------------------------ parsing

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(Decimal),
    Name(String),
    Bracket(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '+' | '-' | '*' | '/' | '×' | '÷' => {
                out.push(Tok::Op(match c {
                    '×' => '*',
                    '÷' => '/',
                    c => c,
                }));
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' | ';' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '[' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&c| c == ']')
                    .ok_or("a [ has no matching ]")?;
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                if name.trim().is_empty() {
                    return Err("empty [ ]".into());
                }
                out.push(Tok::Bracket(name.trim().to_string()));
                i += end + 2;
            }
            c if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Num(Decimal::from_str(&s).map_err(|_| format!("bad number {s:?}"))?));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                out.push(Tok::Name(chars[start..i].iter().collect()));
            }
            '=' if i == 0 || chars[..i].iter().all(|c| c.is_whitespace()) => i += 1, // leading "=" like Excel
            other => return Err(format!("unexpected {other:?}")),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let right = self.term()?;
            let op = if op == '+' { BinOp::Add } else { BinOp::Sub };
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while let Some(Tok::Op(op @ ('*' | '/'))) = self.peek().cloned() {
            self.pos += 1;
            let right = self.unary()?;
            let op = if op == '*' { BinOp::Mul } else { BinOp::Div };
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if let Some(Tok::Op('-')) = self.peek() {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if let Some(Tok::Op('+')) = self.peek() {
            self.pos += 1;
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::Bracket(n)) => Ok(Expr::Display(n)),
            Some(Tok::LParen) => {
                let e = self.expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err("missing )".into()),
                }
            }
            Some(Tok::Name(name)) => {
                if let Some(Tok::LParen) = self.peek() {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if let Some(Tok::RParen) = self.peek() {
                        self.pos += 1;
                    } else {
                        loop {
                            args.push(self.expr()?);
                            match self.next() {
                                Some(Tok::Comma) => continue,
                                Some(Tok::RParen) => break,
                                _ => return Err(format!("missing ) after {name}(…")),
                            }
                        }
                    }
                    Ok(Expr::Call(name.to_lowercase(), args))
                } else {
                    Ok(Expr::Name(name))
                }
            }
            Some(t) => Err(format!("unexpected {}", tok_text(&t))),
            None => Err("formula ends too soon".into()),
        }
    }
}

fn tok_text(t: &Tok) -> String {
    match t {
        Tok::Num(n) => n.to_string(),
        Tok::Name(n) => n.clone(),
        Tok::Bracket(n) => format!("[{n}]"),
        Tok::Op(c) => c.to_string(),
        Tok::LParen => "(".into(),
        Tok::RParen => ")".into(),
        Tok::Comma => ",".into(),
    }
}

pub const FUNCTIONS: &[&str] = &["to", "coalesce", "round", "abs", "min", "max", "if"];

pub fn parse(src: &str) -> Result<Expr, String> {
    let toks = tokenize(src)?;
    if toks.is_empty() {
        return Err("formula is empty".into());
    }
    let mut p = Parser { toks, pos: 0 };
    let e = p.expr()?;
    if let Some(t) = p.peek() {
        return Err(format!("unexpected {}", tok_text(t)));
    }
    check_calls(&e)?;
    Ok(e)
}

fn check_calls(e: &Expr) -> Result<(), String> {
    match e {
        Expr::Call(name, args) => {
            // Other names are user functions, checked when evaluated.
            if !FUNCTIONS.contains(&name.as_str()) {
                return args.iter().try_for_each(check_calls);
            }
            let ok = match name.as_str() {
                "to" => args.len() == 2,
                "round" => args.len() == 1 || args.len() == 2,
                "abs" => args.len() == 1,
                "if" => args.len() == 3,
                _ => !args.is_empty(),
            };
            if !ok {
                return Err(format!("wrong number of arguments for {name}()"));
            }
            args.iter().try_for_each(check_calls)
        }
        Expr::Neg(a) => check_calls(a),
        Expr::Bin(_, a, b) => check_calls(a).and(check_calls(b)),
        _ => Ok(()),
    }
}

/// A column as formulas see it.
#[derive(Clone, Debug)]
pub struct ColRef {
    /// Invariant ID, e.g. `c4`.
    pub id: String,
    /// Variable name, e.g. `price_uah`.
    pub var: String,
    /// Display name, e.g. `Price (UAH)`.
    pub display: String,
}

fn is_currency_code(n: &str) -> bool {
    n.len() == 3 && n.chars().all(|c| c.is_ascii_uppercase())
}

/// Resolve names to column IDs and currencies: `c4` or `price_uah` (any case) or
/// `[Price (UAH)]` is a column; three capital letters are a currency.
pub fn resolve(e: &Expr, cols: &[ColRef]) -> Result<Expr, String> {
    resolve_in(e, cols, false)
}

/// `currency_slot`: the second argument of `to()`, where three letters are
/// always a currency, in any case (`to(c3, usd)`).
fn resolve_in(e: &Expr, cols: &[ColRef], currency_slot: bool) -> Result<Expr, String> {
    let as_currency = |n: &str| -> Option<Expr> {
        if n.len() == 3 && n.chars().all(|c| c.is_ascii_alphabetic()) {
            Currency::new(n).ok().map(Expr::Cur)
        } else {
            None
        }
    };
    let by_name = |n: &str| -> Result<Expr, String> {
        if currency_slot {
            if let Some(c) = as_currency(n) {
                return Ok(c);
            }
        }
        if let Some(c) = cols.iter().find(|c| c.id == n || c.var == n) {
            return Ok(Expr::Col(c.id.clone()));
        }
        if is_currency_code(n) {
            return Ok(Expr::Cur(Currency::new(n).map_err(|e| e.to_string())?));
        }
        if let Some(c) = cols.iter().find(|c| c.var.eq_ignore_ascii_case(n) || c.id.eq_ignore_ascii_case(n)) {
            return Ok(Expr::Col(c.id.clone()));
        }
        // Not a column: three letters are a currency, whatever the case.
        if let Some(c) = as_currency(n) {
            return Ok(c);
        }
        Err(format!("no column called {n}"))
    };
    Ok(match e {
        Expr::Name(n) => by_name(n)?,
        Expr::Display(n) => match cols.iter().find(|c| c.display.eq_ignore_ascii_case(n)) {
            Some(c) => Expr::Col(c.id.clone()),
            None => return Err(format!("no column called [{n}]")),
        },
        Expr::Neg(a) => Expr::Neg(Box::new(resolve_in(a, cols, false)?)),
        Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(resolve_in(a, cols, false)?), Box::new(resolve_in(b, cols, false)?)),
        Expr::Call(f, args) => Expr::Call(
            f.clone(),
            args.iter()
                .enumerate()
                .map(|(i, a)| resolve_in(a, cols, f == "to" && i == 1))
                .collect::<Result<_, _>>()?,
        ),
        other => other.clone(),
    })
}

/// Resolve a formula written for the earlier format, where bare names were
/// column names (case-insensitive) and only `to()`'s second argument was a currency.
pub fn resolve_legacy(e: &Expr, cols: &[ColRef]) -> Result<Expr, String> {
    fn walk(e: &Expr, cols: &[ColRef], currency_slot: bool) -> Result<Expr, String> {
        Ok(match e {
            Expr::Name(n) | Expr::Display(n) => {
                if currency_slot && n.len() == 3 {
                    if let Ok(c) = Currency::new(n) {
                        return Ok(Expr::Cur(c));
                    }
                }
                match cols.iter().find(|c| c.display.eq_ignore_ascii_case(n) || c.var.eq_ignore_ascii_case(n)) {
                    Some(c) => Expr::Col(c.id.clone()),
                    None => match Currency::new(n) {
                        Ok(c) if n.len() == 3 => Expr::Cur(c),
                        _ => return Err(format!("no column called {n}")),
                    },
                }
            }
            Expr::Neg(a) => Expr::Neg(Box::new(walk(a, cols, false)?)),
            Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(walk(a, cols, false)?), Box::new(walk(b, cols, false)?)),
            Expr::Call(f, args) => Expr::Call(
                f.clone(),
                args.iter().enumerate().map(|(i, a)| walk(a, cols, f == "to" && i == 1)).collect::<Result<_, _>>()?,
            ),
            other => other.clone(),
        })
    }
    walk(e, cols, false)
}

/// Column IDs a resolved formula uses.
pub fn refs(e: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Col(id) => out.push(id.clone()),
            Expr::Neg(a) => walk(a, out),
            Expr::Bin(_, a, b) => {
                walk(a, out);
                walk(b, out);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
            _ => {}
        }
    }
    walk(e, &mut out);
    out
}

/// A resolved formula written with variable names, for showing to people.
pub fn render(e: &Expr, cols: &[ColRef]) -> String {
    fn named(e: &Expr, cols: &[ColRef]) -> Expr {
        match e {
            Expr::Col(id) => match cols.iter().find(|c| &c.id == id) {
                Some(c) => Expr::Name(c.var.clone()),
                None => Expr::Name(id.clone()),
            },
            Expr::Neg(a) => Expr::Neg(Box::new(named(a, cols))),
            Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(named(a, cols)), Box::new(named(b, cols))),
            Expr::Call(f, args) => Expr::Call(f.clone(), args.iter().map(|a| named(a, cols)).collect()),
            other => other.clone(),
        }
    }
    named(e, cols).to_string()
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn prec(e: &Expr) -> u8 {
            match e {
                Expr::Bin(BinOp::Add | BinOp::Sub, ..) => 1,
                Expr::Bin(BinOp::Mul | BinOp::Div, ..) => 2,
                _ => 3,
            }
        }
        match self {
            Expr::Num(n) => write!(f, "{n}"),
            Expr::Param(i) => write!(f, "${i}"),
            Expr::Name(n) | Expr::Col(n) => write!(f, "{n}"),
            Expr::Display(n) => write!(f, "[{n}]"),
            Expr::Cur(c) => write!(f, "{c}"),
            Expr::Neg(a) => {
                if prec(a) < 3 {
                    write!(f, "-({a})")
                } else {
                    write!(f, "-{a}")
                }
            }
            Expr::Bin(op, a, b) => {
                let p = prec(self);
                let (sym, right_strict) = match op {
                    BinOp::Add => ("+", false),
                    BinOp::Sub => ("-", true),
                    BinOp::Mul => ("*", false),
                    BinOp::Div => ("/", true),
                };
                let wrap = |e: &Expr, strict: bool| {
                    if prec(e) < p || (strict && prec(e) == p) {
                        format!("({e})")
                    } else {
                        e.to_string()
                    }
                };
                write!(f, "{} {sym} {}", wrap(a, false), wrap(b, right_strict))
            }
            Expr::Call(name, args) => {
                let a: Vec<String> = args.iter().map(|x| x.to_string()).collect();
                write!(f, "{name}({})", a.join(", "))
            }
        }
    }
}

// ------------------------------------------------------------------ evaluation

pub fn eval(e: &Expr, ctx: &dyn Context) -> Value {
    match e {
        Expr::Num(n) => Value::Number(*n),
        Expr::Col(id) => ctx.column(id).unwrap_or_else(|| Value::Error(format!("column {id} no longer exists"))),
        Expr::Cur(c) => Value::Currency(c.clone()),
        Expr::Param(i) => ctx.param(*i).unwrap_or(Value::Error("missing parameter".into())),
        Expr::Name(n) => Value::Error(format!("no column called {n}")),
        Expr::Display(n) => Value::Error(format!("no column called [{n}]")),
        Expr::Neg(a) => match eval(a, ctx) {
            Value::Number(n) => Value::Number(-n),
            Value::Money(n, c) => Value::Money(-n, c),
            Value::Empty => Value::Empty,
            Value::Error(e) => Value::Error(e),
            other => Value::Error(format!("cannot negate {}", describe(&other))),
        },
        Expr::Bin(op, a, b) => binary(*op, eval(a, ctx), eval(b, ctx)),
        Expr::Call(name, args) => call(name, args, ctx),
    }
}

fn describe(v: &Value) -> String {
    match v {
        Value::Empty => "an empty cell".into(),
        Value::Number(_) => "a number".into(),
        Value::Money(_, c) => format!("an amount in {c}"),
        Value::Text(t) => format!("text {t:?}"),
        Value::Currency(c) => format!("the currency {c}"),
        Value::Error(e) => e.clone(),
    }
}

fn binary(op: BinOp, a: Value, b: Value) -> Value {
    use Value::*;
    match (&a, &b) {
        (Error(e), _) | (_, Error(e)) => return Error(e.clone()),
        (Empty, _) | (_, Empty) => return Empty,
        _ => {}
    }
    let div = |x: Decimal, y: Decimal| if y.is_zero() { None } else { x.checked_div(y) };
    match (op, a, b) {
        (BinOp::Add, Number(x), Number(y)) => Number(x + y),
        (BinOp::Sub, Number(x), Number(y)) => Number(x - y),
        (BinOp::Mul, Number(x), Number(y)) => Number(x * y),
        (BinOp::Div, Number(x), Number(y)) => div(x, y).map_or(Error("division by zero".into()), Number),

        (BinOp::Add | BinOp::Sub, Money(x, c1), Money(y, c2)) => {
            if c1 != c2 {
                Error(format!("cannot add {c1} and {c2}; convert one with to()"))
            } else if op == BinOp::Add {
                Money(x + y, c1)
            } else {
                Money(x - y, c1)
            }
        }
        (BinOp::Mul, Money(x, c), Number(y)) | (BinOp::Mul, Number(y), Money(x, c)) => Money(x * y, c),
        (BinOp::Div, Money(x, c), Number(y)) => div(x, y).map_or(Error("division by zero".into()), |v| Money(v, c)),
        (BinOp::Div, Money(x, c1), Money(y, c2)) if c1 == c2 => {
            div(x, y).map_or(Error("division by zero".into()), Number)
        }
        (op, a, b) => Error(format!(
            "cannot {} {} and {}",
            match op {
                BinOp::Add => "add",
                BinOp::Sub => "subtract",
                BinOp::Mul => "multiply",
                BinOp::Div => "divide",
            },
            describe(&a),
            describe(&b)
        )),
    }
}

fn call(name: &str, args: &[Expr], ctx: &dyn Context) -> Value {
    match name {
        "coalesce" => {
            // The first value that is filled in; a problem (say, a missing rate) is
            // reported only if nothing later fills the gap.
            let mut first_error = None;
            for a in args {
                match eval(a, ctx) {
                    Value::Empty => continue,
                    Value::Error(e) => {
                        first_error.get_or_insert(e);
                    }
                    v => return v,
                }
            }
            first_error.map_or(Value::Empty, Value::Error)
        }
        "to" => {
            let target = match eval(&args[1], ctx) {
                Value::Currency(c) => c,
                Value::Text(t) => match Currency::new(&t) {
                    Ok(c) => c,
                    Err(_) => return Value::Error(format!("{t:?} is not a currency")),
                },
                other => return Value::Error(format!("to() needs a currency, not {}", describe(&other))),
            };
            match eval(&args[0], ctx) {
                Value::Empty => Value::Empty,
                Value::Money(x, from) => match ctx.convert(x, &from, &target) {
                    Ok(v) => Value::Money(v, target),
                    Err(e) => Value::Error(e),
                },
                Value::Error(e) => Value::Error(e),
                other => Value::Error(format!("to() converts amounts with a currency, not {}", describe(&other))),
            }
        }
        "round" => {
            let v = eval(&args[0], ctx);
            let places = match args.get(1).map(|a| eval(a, ctx)) {
                None => None,
                Some(Value::Number(n)) if n >= Decimal::ZERO && n <= Decimal::from(12) && n.fract().is_zero() => {
                    Some(n.trunc().to_string().parse::<u32>().unwrap_or(2))
                }
                Some(other) => return Value::Error(format!("round() places must be 0–12, not {}", describe(&other))),
            };
            let r = |x: Decimal, dp: u32| x.round_dp_with_strategy(dp, rust_decimal::RoundingStrategy::MidpointAwayFromZero);
            match v {
                Value::Number(x) => Value::Number(r(x, places.unwrap_or(0))),
                Value::Money(x, c) => {
                    let dp = places.unwrap_or_else(|| c.minor_units());
                    Value::Money(r(x, dp), c)
                }
                other => other,
            }
        }
        "abs" => match eval(&args[0], ctx) {
            Value::Number(x) => Value::Number(x.abs()),
            Value::Money(x, c) => Value::Money(x.abs(), c),
            other => other,
        },
        "min" | "max" => {
            let mut best: Option<Value> = None;
            for a in args {
                let v = eval(a, ctx);
                match (&v, &best) {
                    (Value::Empty, _) => continue,
                    (Value::Error(_), _) => return v,
                    (_, None) => best = Some(v),
                    (_, Some(b)) => {
                        let cmp = match (&v, b) {
                            (Value::Number(x), Value::Number(y)) => x.cmp(y),
                            (Value::Money(x, c1), Value::Money(y, c2)) if c1 == c2 => x.cmp(y),
                            _ => return Value::Error(format!("{name}() needs values of one kind")),
                        };
                        let take = if name == "min" { cmp.is_lt() } else { cmp.is_gt() };
                        if take {
                            best = Some(v);
                        }
                    }
                }
            }
            best.unwrap_or(Value::Empty)
        }
        "if" => {
            let truthy = match eval(&args[0], ctx) {
                Value::Empty => false,
                Value::Number(n) | Value::Money(n, _) => !n.is_zero(),
                Value::Text(t) => !t.is_empty(),
                Value::Currency(_) => true,
                Value::Error(e) => return Value::Error(e),
            };
            eval(if truthy { &args[1] } else { &args[2] }, ctx)
        }
        other => {
            let Some((f, body)) = ctx.functions().and_then(|fs| fs.get(other)) else {
                return Value::Error(format!("unknown function {other}()"));
            };
            if args.len() != f.params.len() {
                return Value::Error(format!("{other}() takes {} argument(s): {}", f.params.len(), f.params.join(", ")));
            }
            if ctx.depth() >= MAX_DEPTH {
                return Value::Error(format!("{other}(): functions call each other in a loop"));
            }
            let values = args.iter().map(|a| eval(a, ctx)).collect();
            eval(body, &Call { args: values, parent: ctx })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    struct Row(HashMap<String, Value>);

    impl Context for Row {
        fn column(&self, id: &str) -> Option<Value> {
            self.0.get(id).cloned()
        }
        fn convert(&self, amount: Decimal, from: &Currency, to: &Currency) -> Result<Decimal, String> {
            match (from.code(), to.code()) {
                ("UAH", "USD") => Ok(amount / dec!(44)),
                _ => Err(format!("no {from}→{to} rate")),
            }
        }
    }

    fn c(s: &str) -> Currency {
        Currency::new(s).unwrap()
    }

    fn cols() -> Vec<ColRef> {
        [("c1", "item", "Item"), ("c4", "price_uah", "Price (UAH)"), ("c5", "price_usd", "Price (USD)"), ("c6", "unit_usd", "Unit (USD)"), ("c7", "qty", "Qty")]
            .iter()
            .map(|(id, var, display)| ColRef { id: id.to_string(), var: var.to_string(), display: display.to_string() })
            .collect()
    }

    fn compile(src: &str) -> Expr {
        resolve(&parse(src).unwrap(), &cols()).unwrap()
    }

    fn row(pairs: &[(&str, Value)]) -> Row {
        Row(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn three_ways_to_refer_to_a_column() {
        let a = compile("coalesce(to(price_uah, USD), price_usd)");
        let b = compile("coalesce(to(c4, USD), c5)");
        let d = compile("coalesce(to([Price (UAH)], USD), [price (usd)])");
        assert_eq!(a, b);
        assert_eq!(a, d);
        // Stored with IDs, shown with variable names.
        assert_eq!(a.to_string(), "coalesce(to(c4, USD), c5)");
        assert_eq!(render(&a, &cols()), "coalesce(to(price_uah, USD), price_usd)");
        // Case is forgiven for variable names, but capitals of three letters are a currency.
        assert_eq!(compile("Qty * 2"), compile("qty * 2"));
        assert!(matches!(compile("to(price_uah, USD)"), Expr::Call(_, ref a) if a[1] == Expr::Cur(c("USD"))));
    }

    #[test]
    fn currencies_in_any_case() {
        assert_eq!(compile("coalesce(to(c4, usd), c5)"), compile("coalesce(to(c4, USD), c5)"));
        assert_eq!(compile("to(price_uah, eur)").to_string(), "to(c4, EUR)");
    }

    #[test]
    fn unknown_names_are_reported() {
        assert_eq!(resolve(&parse("price * qty").unwrap(), &cols()).unwrap_err(), "no column called price");
        assert!(resolve(&parse("[Nope]").unwrap(), &cols()).is_err());
    }

    #[test]
    fn unit_cost_from_either_price() {
        let f = compile("coalesce(to(price_uah, USD), price_usd)");
        let a = row(&[("c4", Value::Money(dec!(3300), c("UAH"))), ("c5", Value::Empty)]);
        assert_eq!(eval(&f, &a), Value::Money(dec!(75), c("USD")));
        let b = row(&[("c4", Value::Empty), ("c5", Value::Money(dec!(250), c("USD")))]);
        assert_eq!(eval(&f, &b), Value::Money(dec!(250), c("USD")));
    }

    #[test]
    fn missing_rate_is_not_swallowed() {
        let f = compile("coalesce(to(price_uah, EUR), price_usd)");
        let a = row(&[("c4", Value::Money(dec!(100), c("UAH"))), ("c5", Value::Empty)]);
        assert!(matches!(eval(&f, &a), Value::Error(e) if e.contains("UAH→EUR")));
    }

    #[test]
    fn line_total_and_empty_quantity() {
        let f = compile("= unit_usd * qty");
        let r = row(&[("c6", Value::Money(dec!(30.80), c("USD"))), ("c7", Value::Number(dec!(4)))]);
        assert_eq!(eval(&f, &r), Value::Money(dec!(123.20), c("USD")));
        let empty = row(&[("c6", Value::Money(dec!(30.80), c("USD"))), ("c7", Value::Empty)]);
        assert_eq!(eval(&f, &empty), Value::Empty);
    }

    #[test]
    fn mixing_currencies_is_an_error() {
        let f = compile("price_uah + price_usd");
        let r = row(&[("c4", Value::Money(dec!(1), c("UAH"))), ("c5", Value::Money(dec!(1), c("USD")))]);
        assert!(matches!(eval(&f, &r), Value::Error(e) if e.contains("convert")));
    }

    #[test]
    fn parse_errors_are_readable() {
        assert!(parse("").is_err());
        assert!(parse("qty *").unwrap_err().contains("too soon"));
        assert!(parse("to(qty)").unwrap_err().contains("arguments"));
        assert!(parse("(1 + 2").unwrap_err().contains(")"));
    }

    #[test]
    fn legacy_formulas_resolve_by_display_name() {
        let legacy = [("c1", "uah", "UAH"), ("c2", "usd", "USD"), ("c3", "unit_usd", "Unit USD"), ("c4", "qty", "Qty")]
            .iter()
            .map(|(id, var, display)| ColRef { id: id.to_string(), var: var.to_string(), display: display.to_string() })
            .collect::<Vec<_>>();
        let f = resolve_legacy(&parse("coalesce(to(UAH, USD), USD)").unwrap(), &legacy).unwrap();
        assert_eq!(f.to_string(), "coalesce(to(c1, USD), c2)");
        let g = resolve_legacy(&parse("[Unit USD] * Qty").unwrap(), &legacy).unwrap();
        assert_eq!(g.to_string(), "c3 * c4");
    }

    #[test]
    fn display_round_trip() {
        assert_eq!(parse("(a - b) - (c - d)").unwrap().to_string(), "a - b - (c - d)");
        let f = compile("round(price_uah / 3)");
        assert_eq!(resolve(&parse(&f.to_string()).unwrap(), &cols()).unwrap(), f);
    }

    struct WithFns(Row, Functions);
    impl Context for WithFns {
        fn column(&self, id: &str) -> Option<Value> {
            self.0.column(id)
        }
        fn convert(&self, a: Decimal, f: &Currency, t: &Currency) -> Result<Decimal, String> {
            self.0.convert(a, f, t)
        }
        fn functions(&self) -> Option<&Functions> {
            Some(&self.1)
        }
    }

    fn uf(name: &str, params: &[&str], body: &str) -> UserFunction {
        UserFunction { name: name.into(), params: params.iter().map(|p| p.to_string()).collect(), body: body.into(), description: String::new() }
    }

    #[test]
    fn user_functions() {
        let (fns, problems) = Functions::new([
            uf("vat", &["x"], "x * 0.2"),
            uf("with_vat", &["x"], "x + vat(x)"),
            uf("usd", &["x"], "to(x, USD)"),
            uf("bad", &["x"], "x * qty"),
            uf("ping", &["x"], "pong(x)"),
            uf("pong", &["x"], "ping(x)"),
        ]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("qty is not a parameter"));
        assert!(fns.has_loop("ping") && !fns.has_loop("with_vat"));
        let r = WithFns(row(&[("c4", Value::Money(dec!(4400), c("UAH"))), ("c7", Value::Number(dec!(2)))]), fns);
        assert_eq!(eval(&compile("with_vat(price_uah)"), &r), Value::Money(dec!(5280.0), c("UAH")));
        assert_eq!(eval(&compile("usd(price_uah) * qty"), &r), Value::Money(dec!(200), c("USD")));
        assert!(matches!(eval(&compile("vat(1, 2)"), &r), Value::Error(e) if e.contains("takes 1")));
        assert!(matches!(eval(&compile("ping(1)"), &r), Value::Error(e) if e.contains("loop")));
        assert!(matches!(eval(&compile("nope(1)"), &r), Value::Error(e) if e.contains("unknown function")));
        assert!(compile_function(&uf("round", &["x"], "x")).is_err());
    }

    #[test]
    fn rounding_uses_currency_decimals() {
        let f = compile("round(price_uah / 3)");
        let r = row(&[("c4", Value::Money(dec!(10), c("UAH")))]);
        assert_eq!(eval(&f, &r), Value::Money(dec!(3.33), c("UAH")));
    }
}
