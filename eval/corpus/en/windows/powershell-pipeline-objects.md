# PowerShell pipeline: objects, not text

Notes for colleagues coming from bash. The single biggest mental shift in PowerShell is
that the pipeline carries .NET objects with typed properties, not lines of text. Once that
clicks, most "how do I awk this" questions disappear; until it clicks, scripts are full of
fragile string parsing that PowerShell never needed.

## The basic idea

In bash, `ps aux | grep node | awk '{print $2}'` works on columns of text. The column
layout is a contract between tools, and it breaks when a field contains a space.

In PowerShell:

```powershell
Get-Process node | Select-Object Id, ProcessName, WorkingSet64
```

`Get-Process` emits `System.Diagnostics.Process` objects. `Select-Object` picks
properties by name. No columns, no splitting, no regex.

Filtering and sorting work on properties with their real types:

```powershell
Get-ChildItem -Recurse -File |
  Where-Object Length -gt 100MB |
  Sort-Object Length -Descending |
  Select-Object -First 10 FullName, @{n='MB'; e={[math]::Round($_.Length / 1MB, 1)}}
```

`Length -gt 100MB` compares numbers, not strings: `100MB` is a numeric literal, and the
comparison does not depend on how the console happens to format sizes.

## Discovering what an object has

```powershell
Get-Service | Get-Member              # properties and methods
Get-Service spooler | Format-List *   # every property with its value
(Get-Item .\app.exe).VersionInfo
```

`Get-Member` is the equivalent of reading the man page for the output format, except it is
always accurate.

## Formatting is the last step

`Format-Table`, `Format-List` and `Format-Wide` turn objects into formatting instructions
for the host. After them, you no longer have the original objects, so anything piped
further gets garbage:

```powershell
# wrong: Export-Csv receives formatting records, not services
Get-Service | Format-Table Name, Status | Export-Csv services.csv

# right: shape with Select-Object, format or export at the end
Get-Service | Select-Object Name, Status | Export-Csv services.csv -NoTypeInformation
```

Rule: `Format-*` only at the very end of an interactive pipeline, never in a script whose
output is consumed by something else.

## Getting a plain value out

`Select-Object Name` still returns objects that have one property called `Name`. To get the
raw strings use `-ExpandProperty` or member enumeration:

```powershell
$names = Get-Service | Select-Object -ExpandProperty Name
$names = (Get-Service).Name          # same result, shorter
```

Forgetting this produces the classic bug where a string interpolates as
`@{Name=spooler}` instead of `spooler`.

## Where text comes back

Native programs (`git`, `ipconfig`, `cargo`) still write text. PowerShell turns their
stdout into an array of strings, one per line. At that boundary you either parse text, or
better, ask the tool for structured output:

```powershell
$status = git status --porcelain=v1           # string[] — stable, parseable
$info   = cargo metadata --format-version 1 | ConvertFrom-Json
$info.packages | Select-Object name, version
```

`ConvertFrom-Json` turns JSON into objects, and from there everything above applies. The
reverse direction has a trap: `ConvertTo-Json` only serializes two levels deep by default
and silently flattens deeper objects to their type name. Pass `-Depth 10` (or more) when
the structure is nested.

To turn objects into a single text blob, for example to send to a logger, use
`Out-String`. For a CSV string without a file, `ConvertTo-Csv`.

## Grouping and measuring

```powershell
Get-ChildItem -Recurse -File |
  Group-Object Extension |
  Sort-Object Count -Descending |
  Select-Object -First 5 Name, Count

Get-ChildItem -Recurse -File | Measure-Object Length -Sum -Average
```

These replace `sort | uniq -c | sort -rn` and `awk '{s+=$1} END {print s}'` and work on
real numbers.

## Parameter binding from the pipeline

Cmdlets do not receive "the previous output" as one blob; PowerShell binds each incoming
object to a parameter. Two mechanisms exist:

- **ByValue** — the whole object is bound if its type matches the parameter type.
  `Get-Service spooler | Stop-Service` works because `Stop-Service -InputObject` accepts
  `ServiceController`.
- **ByPropertyName** — a property of the incoming object is bound to the parameter with the
  same name. `Import-Csv hosts.csv | Test-Connection` works if the CSV has a column called
  `ComputerName` (or an alias of it).

When piping "does nothing", `Trace-Command -Name ParameterBinding -PSHost { ... }` shows
exactly which parameter each object was bound to, or why it was rejected.

## Writing a pipeline-aware function

Your own functions get the same behavior with `ValueFromPipeline` and a `process` block:

```powershell
function Get-FileHashShort {
    param(
        [Parameter(ValueFromPipeline, ValueFromPipelineByPropertyName)]
        [Alias('FullName')]
        [string] $Path
    )
    process {
        $h = Get-FileHash -Path $Path -Algorithm SHA256
        [pscustomobject]@{ Path = $Path; Hash = $h.Hash.Substring(0, 12) }
    }
}

Get-ChildItem *.dll | Get-FileHashShort | Sort-Object Hash
```

Key points: the `process` block runs once per input object (without it, only the last
object is processed); the `FullName` alias lets `FileInfo` objects bind by property name;
and emitting `[pscustomobject]` rather than formatted strings keeps the output usable by
the next command.

## Performance notes

- The pipeline streams one object at a time. That keeps memory low but adds per-object
  overhead. For hot loops over many items, `foreach ($x in $items) { }` is much faster
  than `$items | ForEach-Object { }`.
- `Where-Object` with a script block is slower than the simplified syntax
  (`Where-Object Length -gt 1MB`), and both are slower than filtering at the source:
  `Get-ChildItem -Filter *.log` lets the file system do the work.
- In PowerShell 7, `ForEach-Object -Parallel` runs the block in parallel runspaces;
  useful for I/O-bound work like hitting many URLs, wasteful for tiny CPU work.
- Appending to an array with `+=` in a loop copies the array each time. Collect output
  from the loop directly (`$result = foreach (...) { ... }`) or use a
  `System.Collections.Generic.List[object]`.

## A small comparison table

| Task | bash habit | PowerShell way |
|---|---|---|
| Pick a column | `awk '{print $2}'` | `Select-Object -ExpandProperty Id` |
| Filter rows | `grep foo` | `Where-Object Name -like '*foo*'` |
| Count by key | `sort \| uniq -c` | `Group-Object Key` |
| Sum a field | `awk '{s+=$5} END{print s}'` | `Measure-Object Length -Sum` |
| Parse JSON | `jq .name` | `ConvertFrom-Json` then `.name` |
| Search text in files | `grep -rn pattern .` | `Select-String -Pattern pattern -Path *.txt` |

## Takeaways

Think in properties, not columns. Shape data with `Select-Object`, filter with
`Where-Object`, and format only at the end. When crossing into native tools, prefer their
JSON or porcelain output and convert back to objects as early as possible.
