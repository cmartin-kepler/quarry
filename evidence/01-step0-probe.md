# 01 — Step-0 claim-level probe

**Question.** On born-digital tables, does the cheap parse produce *wrong answers*,
or is its messiness cosmetic to a consumer? (The plan's riskiest assumption.)

**Harness.** `scripts/probe.py` — for a set of table pages, compare the cheap
parse's numeric content against Docling (reference), matched table-to-table by bbox
IoU. Numeric *multiset* agreement = "every total / cell-lookup / delta answer is
identical."

## Run A — cheap = pdfplumber `extract_tables`

```
uv run scripts/probe.py --pdf input/finance/brk-2023-ar.pdf --pages auto --limit 8
```
Pages auto-selected: `[1, 42, 54, 55, 56, 57, 59, 61]`.

- cheap: 10 tables, docling: 7, **matched: 6**, **answer-faithful: 3/6 (50%)**.
- Every divergence was the same cause: pdfplumber's table-finder **exploded columns**
  and split `$`/sign from digits — e.g. page 59 cheap shape `11×12` vs docling `11×4`;
  `$ (30)` became cells `$`,`(30`,`)`, and a naive read recovers `30` (sign lost).

**Caveat that this run surfaced:** pdfplumber's `extract_tables` is the *old
front-end the plan replaces*. So Run A mostly re-confirms pdfplumber is bad, not the
new path. → swap the cheap side to the plan's real path.

## Run B — cheap = YOLO region + litparse (the plan's easy path)

```
uv run scripts/probe.py --pdf input/finance/brk-2023-ar.pdf --pages auto --limit 8
```
(cheap = YOLO table region → litparse over the text layer; numbers from litparse word
tokens, which keep `$ (30)` intact.)

- cheap: 7 tables, docling: 7, **matched: 7**, **answer-faithful: 1/7 (14%)**.

### Adjudicating the divergences

litparse tokens on page 59 were **truncated/missing**, e.g.:
```
litparse:  ... '9,567', '6,484', '4,8',  'BNSF', '5,087', '5,946', '5,9', ...
docling:   ... '9,567', '6,484', '4,807',          '5,087', '5,946', '5,990', ...
```
`4,807`→`4,8`, `5,990`→`5,9`, and the year header (`2023 2022 2021`) + first data row
(`$ 5,428  $ (30)  $ 870`) entirely missing.

`quarry region-check` + a direct geometry check on page 59 (`pdfplumber`):
- YOLO box = `[44, 116.8, 549.4, 245.6]`.
- rightmost column `4,807` spans `x0=539.5 → x1=562.0` (center 550.7 **>** box right
  549.4 → clipped); first data row at `top=108.5` **<** box top 116.8 (above the box).
- **The page text layer has the full values** — XY-cut block of the lower page (235
  words) contains `2023 2022 2021 ... $ 5,428 $ (30) $ 870` intact.

So litparse faithfully parsed a **clipped region**. The box is ~10–15pt too tight on
two edges. Same box on **yolo26n** (`[40.7, 117.5, 547.6, 244.8]`) and **doclayout**
— a *systematic, model-independent, padding-fixable* clip, not a gross miss.

## Conclusions / decisions

1. The cheap-path divergences are **region clipping**, not value errors. Region scope
   is a real failure surface — but subtle (the box looks ~90% right, hence "regions
   were basically fine").
2. **The probe's numeric-multiset is blind to column/row mangling** — right numbers in
   the wrong cells still "match" — and it used litparse's *raw tokens*, bypassing the
   crate's grid coalescer (`grid.rs`), which is the actual mangling suspect. So this
   probe **cannot** support "region is the failure, not column/row." The column/row
   issue is real and is handled by **Stage 2 repair + Stage 3 materialize**, not by
   demanding a perfect parser.
3. The XY-cut cross-check **under-segments dense pages** (whole table = 1 block, default
   gap thresholds) → it flags the page but can't pinpoint the clipped table. The
   precise cross-check for ruled financial tables would be ruling-line/vector geometry
   (built, parked).

**Speed seen in this run** (cheap was startup-bound): cheap 20.2s (YOLO 18.2 reload +
litparse 2.0) vs docling 6.1s; per-table litparse ~0.3s vs docling ~0.88s — see
`02` and `03` for the clean timing.
