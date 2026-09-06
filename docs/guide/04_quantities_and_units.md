# Quantities and Units

A tutorial on the one feature the neighbouring languages do not have:
values with a physical dimension. Distances, durations, data sizes,
and anything you declare yourself are typed by their dimension,
checked in arithmetic, accepted from documents in any unit, and
serialized in a canonical one. The ```decl blocks form one module,
`journey.decl`.

What the page teaches: a dimension is a type, a unit is a scale on it,
arithmetic composes dimensions, and a document may speak in kilometres
while the schema thinks in metres
([03. Types](../specification/03_types.md) §3.16).

## 1. The catalog, and your own

Time, length, mass, and the other SI base dimensions, their units, and
every prefixed form are ambient: `12km`, `15min`, `2TiB`, `250ms` are
literals ([13. Standard library](../specification/13_stdlib.md)
§13.10). The catalog stops at the SI units, so a minute and an hour are
yours to declare, as are dimensions the catalog does not name:

```decl
dimension Speed = Length / Time
unit mps: Speed
unit kph = (1000.0 / 3600.0) mps
unit min = 60 s
unit h = 3600 s
```

- `dimension Speed = Length / Time` names a derived dimension;
  dimensions form a group, and `Length / Time` equals
  `Length * Time ^ -1`.
- `unit mps: Speed` is the base unit of that dimension; `kph` is a
  constant multiple of it. The factor is a constant expression — and
  it is a float expression on purpose: `1000 / 3600` would be integer
  division ([04. Expressions](../specification/04_expressions.md) §4.4).
- Units and dimensions live in their own name spaces: declaring `unit
  min` does not stop you from naming a value `min`.

## 2. Arithmetic

```decl
type Leg = {
    distance: quantity<Length>
    duration: quantity<Time>
    speed = distance / duration
    assert plausible: speed <= 130kph
        else warn `a leg faster than 130 km/h`
}
```

`distance / duration` has type `quantity<Length / Time>`, which is
`quantity<Speed>`; the comparison with `130kph` is between equal
dimensions and converts under the hood. Comparing a speed with a
distance would be a type error at check time, not a wrong answer at
run time.

```decl
type Journey = {
    legs: Leg[1..64]
    total_distance = std.array.fold(legs, 0m, (a, l) => a + l.distance)
    total_time = std.array.fold(legs, 0s, (a, l) => a + l.duration)
    average = total_distance / total_time
    average_kph = average / 1kph
}
```

- The folds add quantities; the seed fixes the dimension of the sum.
- `average / 1kph` divides a speed by a speed: the dimension cancels
  and the result is a plain `float` — the way to read a quantity in a
  unit of your choosing.

## 3. A value, and what it prints

```decl
export output commute: Journey = {
    legs: [
        { distance: 12km, duration: 15min }
        { distance: 800m, duration: 10min }
    ]
}
```

`decl evaluate journey.decl` prints every quantity in the base unit of
its dimension — metres, seconds, and `mps` for the dimension declared
here — as the interchange object `{ "value", "unit" }`:

```json
{
  "commute": {
    "legs": [
      { "distance": { "value": 12000.0, "unit": "m" },
        "duration": { "value": 900.0, "unit": "s" },
        "speed": { "value": 13.333333333333334, "unit": "mps" } },
      { "distance": { "value": 800.0, "unit": "m" },
        "duration": { "value": 600.0, "unit": "s" },
        "speed": { "value": 1.3333333333333333, "unit": "mps" } }
    ],
    "total_distance": { "value": 12800.0, "unit": "m" },
    "total_time": { "value": 1500.0, "unit": "s" },
    "average": { "value": 8.533333333333333, "unit": "mps" },
    "average_kph": 30.72
  }
}
```

Magnitudes are IEEE 754 doubles, converted to the base unit by an
exact rational scaling with one rounding, so `12km` is exactly
`12000.0` and every implementation prints the same digits
([09. Semantics](../specification/09_semantics.md) §9.5).

## 4. A document in its own units

```decl
export input trip: Journey
```

`trip.json` speaks in kilometres, hours, and minutes:

```json
{
  "legs": [
    { "distance": { "value": 320, "unit": "km" }, "duration": { "value": 2.5, "unit": "h" } },
    { "distance": { "value": 5, "unit": "km" }, "duration": { "value": 12, "unit": "min" } }
  ]
}
```

```bash
decl evaluate journey.decl --input trip=trip.json --output trip
```

```json
{
  "legs": [
    { "distance": { "value": 320000.0, "unit": "m" },
      "duration": { "value": 9000.0, "unit": "s" },
      "speed": { "value": 35.55555555555556, "unit": "mps" } },
    { "distance": { "value": 5000.0, "unit": "m" },
      "duration": { "value": 720.0, "unit": "s" },
      "speed": { "value": 6.944444444444445, "unit": "mps" } }
  ],
  "total_distance": { "value": 325000.0, "unit": "m" },
  "total_time": { "value": 9720.0, "unit": "s" },
  "average": { "value": 33.43621399176955, "unit": "mps" },
  "average_kph": 120.37037037037037
}
```

Any unit whose dimension matches is accepted, including `h` and `min`
declared in this module; a document that wrote
`{ "value": 320, "unit": "s" }` for a distance would be rejected at
binding with a dimension mismatch, before any rule ran
([10. Interchange](../specification/10_interchange.md) §10.2). The
first leg averages 128 km/h, under the `plausible` line — change the
duration to `2` hours and `validate` reports the warning at
`trip.legs[0]`.

## 5. Data sizes

`DataSize` is the one non-SI dimension in the catalog, because the
corpus the language was built against needed it: `bit` is the base
unit, `B` is eight of them, and both take the binary prefixes
(`KiB`, `MiB`, `GiB`, `TiB`) and the decimal ones (`kB`, `MB`, `GB`,
`TB`). A schema that says `memory?: quantity<DataSize> = 4GiB` accepts
`{ "value": 64, "unit": "GiB" }` and `{ "value": 500, "unit": "GB" }`
alike and prints both in bits — which is what
[Validating documents](02_validating_documents.md) does with a machine
inventory.

## Where to go next

- The full rules: dimension algebra, unit declarations, the
  interchange form — [03. Types](../specification/03_types.md) §3.16.
- The generation rule for the catalog's prefixed units —
  [13. Standard library](../specification/13_stdlib.md) §13.10.

---

- Index: [Documentation home](../README.md)
