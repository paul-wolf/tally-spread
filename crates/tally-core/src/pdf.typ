// A sheet as a printable table. Data comes from Rust as JSON in sys.inputs.data.
#let d = json(bytes(sys.inputs.data))

#set page(
  paper: "a4",
  flipped: d.landscape,
  margin: (x: 14mm, top: 14mm, bottom: 16mm),
  footer: context [
    #set text(7pt, fill: luma(120))
    #d.footer
    #h(1fr)
    #counter(page).display("1 / 1", both: true)
  ],
)
#set text(font: ("Inter", "Helvetica Neue", "Helvetica", "Arial", "DejaVu Sans", "Libertinus Serif"), size: 8.5pt)
#set par(justify: false)
#show link: set text(fill: rgb("#2f5fc4"))

#text(15pt, weight: "bold", d.title)
#v(-4pt)
#text(8pt, fill: luma(100), d.subtitle)
#v(6pt)

#let cell(x) = {
  let body = if x.link != none { link(x.link, x.t) } else { x.t }
  if x.b { strong(body) } else { body }
}

#table(
  columns: d.columns.map(c => if c.numeric { auto } else { 1fr * c.weight }),
  align: d.columns.map(c => if c.numeric { right } else { left }),
  stroke: none,
  inset: (x: 5pt, y: 3.6pt),
  fill: (_, y) => {
    if y == 0 { none }
    else {
      let k = d.kinds.at(y - 1, default: "item")
      if k == "section" { luma(236) } else if k == "total" { luma(244) } else { none }
    }
  },
  table.header(..d.columns.map(c => text(weight: "bold", c.name))),
  table.hline(y: 1, stroke: 0.6pt + luma(90)),
  table.hline(y: d.kinds.len(), stroke: 0.6pt + luma(90)),
  ..d.rows.map(r => r.map(cell)).flatten(),
)

#if d.notes.len() > 0 [
  #v(4pt)
  #set text(7.5pt, fill: luma(90))
  #for n in d.notes [#n \ ]
]
