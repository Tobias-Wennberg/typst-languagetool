#let data = (
  "Detta är text med _feeelstavad_ och felstavt ord.",
)
#for i in data {
  eval(i, mode: "markup")
}
