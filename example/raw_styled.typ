#set text(lang: "sv")

#show raw.where(block: true): it => block(
  fill: luma(240),
  inset: 6pt,
)[
  #for line in it.lines {
    line
    linebreak()
  }
]

```yaml
feeelstavad: felstavat
```
