//! Column formulas: one expression computes a column for every row.
//!
//! ```text
//! coalesce(to(UAH, USD), USD)      unit cost in USD, from whichever price is filled in
//! [Unit USD] * Qty                 line total
//! to(Price, GBP)                   convert at the sheet's rates
//! round([Unit USD] * 1.2, 2)
//! ```
//!
//! Names refer to columns (case-insensitive); names with spaces go in brackets.
//! A column letter (`C`) also works. A three-letter code that is not a column is a
//! currency. Amounts keep their currency: adding UAH to USD is an error, never a
//! silent mix; `to()` converts.

use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;

use crate::money::Currency;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(Decimal),
    Name(String),
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
    /// The value of a column in the current row, or `None` if no column has that name.
    fn column(&self, name: &str) -> Option<Value>;
    /// Convert an amount between currencies at the sheet's rates.
    fn convert(&self, amount: Decimal, from: &Currency, to: &Currency) -> Result<Decimal, String>;
}

// ------------------------------------------------------------------ parsing

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(Decimal),
    Name(String),
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
                out.push(Tok::Name(name.trim().to_string()));
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
        Tok::Op(c) => c.to_string(),
        Tok::LParen => "(".into(),
        Tok::RParen => ")".into(),
        Tok::Comma => ",".into(),
    }
}

const FUNCTIONS: &[&str] = &["to", "coalesce", "round", "abs", "min", "max", "if"];

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
            if !FUNCTIONS.contains(&name.as_str()) {
                return Err(format!("unknown function {name}() (available: {})", FUNCTIONS.join(", ")));
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

/// Is this the currency argument of `to(x, CUR)`? It names a currency, not a column.
fn is_currency_arg(f: &str, i: usize, a: &Expr) -> bool {
    f == "to" && i == 1 && matches!(a, Expr::Name(n) if n.len() == 3 && Currency::new(n).is_ok())
}

/// Every column name a formula refers to.
pub fn names(e: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Name(n) => out.push(n.clone()),
            Expr::Neg(a) => walk(a, out),
            Expr::Bin(_, a, b) => {
                walk(a, out);
                walk(b, out);
            }
            Expr::Call(f, args) => {
                for (i, a) in args.iter().enumerate() {
                    if !is_currency_arg(f, i, a) {
                        walk(a, out);
                    }
                }
            }
            Expr::Num(_) => {}
        }
    }
    walk(e, &mut out);
    out
}

/// The formula with one column name replaced (case-insensitive), e.g. after a
/// column rename. Currency codes in `to()` are left alone.
pub fn rename(e: &Expr, old: &str, new: &str) -> Expr {
    match e {
        Expr::Name(n) if n.eq_ignore_ascii_case(old) => Expr::Name(new.to_string()),
        Expr::Neg(a) => Expr::Neg(Box::new(rename(a, old, new))),
        Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(rename(a, old, new)), Box::new(rename(b, old, new))),
        Expr::Call(f, args) => Expr::Call(
            f.clone(),
            args.iter()
                .enumerate()
                .map(|(i, a)| if is_currency_arg(f, i, a) { a.clone() } else { rename(a, old, new) })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn name_text(n: &str) -> String {
    if n.chars().all(|c| c.is_alphanumeric() || c == '_') && !n.starts_with(|c: char| c.is_ascii_digit()) {
        n.to_string()
    } else {
        format!("[{n}]")
    }
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
            Expr::Name(n) => write!(f, "{}", name_text(n)),
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
        Expr::Name(n) => match ctx.column(n) {
            Some(v) => v,
            None => match Currency::new(n) {
                Ok(c) if n.len() == 3 => Value::Currency(c),
                _ => Value::Error(format!("no column named {n}")),
            },
        },
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
            // `to(UAH, USD)`: the second argument is the currency code itself, even
            // when a column happens to be called USD.
            let literal = match &args[1] {
                Expr::Name(n) if n.len() == 3 => Currency::new(n).ok(),
                _ => None,
            };
            let target = match literal.map(Value::Currency).unwrap_or_else(|| eval(&args[1], ctx)) {
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
        other => Value::Error(format!("unknown function {other}()")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    struct Row(HashMap<String, Value>);

    impl Context for Row {
        fn column(&self, name: &str) -> Option<Value> {
            self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
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

    fn row(pairs: &[(&str, Value)]) -> Row {
        Row(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn unit_cost_from_either_price() {
        let f = parse("coalesce(to(UAH, USD), USD)").unwrap();
        let a = row(&[("UAH", Value::Money(dec!(3300), c("UAH"))), ("USD", Value::Empty)]);
        assert_eq!(eval(&f, &a), Value::Money(dec!(75), c("USD")));
        let b = row(&[("UAH", Value::Empty), ("USD", Value::Money(dec!(250), c("USD")))]);
        assert_eq!(eval(&f, &b), Value::Money(dec!(250), c("USD")));
    }

    #[test]
    fn line_total_and_brackets() {
        let f = parse("=[Unit USD] * Qty").unwrap();
        let r = row(&[("Unit USD", Value::Money(dec!(30.80), c("USD"))), ("qty", Value::Number(dec!(4)))]);
        assert_eq!(eval(&f, &r), Value::Money(dec!(123.20), c("USD")));
        let empty = row(&[("Unit USD", Value::Money(dec!(30.80), c("USD"))), ("qty", Value::Empty)]);
        assert_eq!(eval(&f, &empty), Value::Empty);
    }

    #[test]
    fn mixing_currencies_is_an_error() {
        let f = parse("UAH + USD").unwrap();
        let r = row(&[("UAH", Value::Money(dec!(1), c("UAH"))), ("USD", Value::Money(dec!(1), c("USD")))]);
        assert!(matches!(eval(&f, &r), Value::Error(e) if e.contains("convert")));
    }

    #[test]
    fn parse_errors_are_readable() {
        assert!(parse("").is_err());
        assert!(parse("Qty *").unwrap_err().contains("too soon"));
        assert!(parse("sum(Qty)").unwrap_err().contains("unknown function"));
        assert!(parse("to(UAH)").unwrap_err().contains("arguments"));
        assert!(parse("(1 + 2").unwrap_err().contains(")"));
    }

    #[test]
    fn rename_and_display() {
        let f = parse("coalesce(to(UAH, USD), [Price USD]) * qty").unwrap();
        let g = rename(&f, "price usd", "USD price");
        // Renaming a column called USD leaves the currency code in to() alone.
        let h = rename(&parse("coalesce(to(UAH, USD), USD)").unwrap(), "USD", "US price");
        assert_eq!(h.to_string(), "coalesce(to(UAH, USD), [US price])");
        assert_eq!(g.to_string(), "coalesce(to(UAH, USD), [USD price]) * qty");
        assert_eq!(parse("(a - b) - (c - d)").unwrap().to_string(), "a - b - (c - d)");
        assert_eq!(parse(&g.to_string()).unwrap(), g);
    }

    #[test]
    fn rounding_uses_currency_decimals() {
        let f = parse("round(UAH / 3)").unwrap();
        let r = row(&[("UAH", Value::Money(dec!(10), c("UAH")))]);
        assert_eq!(eval(&f, &r), Value::Money(dec!(3.33), c("UAH")));
    }
}
