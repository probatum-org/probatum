## v0.11.0 (2026-09-23)

### BREAKING CHANGE

- the JSON outcome moves from schema 2 to 4. It adds
scenario, step, captures, output_withheld, non-execution reasons and
inferred prerequisites; a new Excluded status appears, and log_file is
null for checks that did not run. Consumers switching on status or
reading log_file unconditionally must handle both.

### Feat

- named scenarios, OS scope and captured values

### Fix

- **release**: give cidx release create what it reads back
- **test**: chunked fixture closes with FIN, not RST

## v0.10.0 (2026-08-15)

## v0.9.0 (2026-08-14)

## v0.8.0 (2026-08-10)

## v0.7.0 (2026-08-10)

## v0.6.1 (2026-08-09)

## v0.6.0 (2026-08-09)

## v0.5.0 (2026-08-08)

## v0.4.0 (2026-08-08)

## v0.3.0 (2026-08-08)

## v0.2.1 (2026-07-28)

## v0.2.0 (2026-07-27)

## v0.1.0 (2026-07-16)
