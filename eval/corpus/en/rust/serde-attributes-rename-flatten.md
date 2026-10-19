# serde attributes I keep looking up: rename_all, deny_unknown_fields, flatten

## rename_all

```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job { job_id: u64, created_at: String }   // {"jobId":..,"createdAt":..}
```

Values: `lowercase`, `UPPERCASE`, `PascalCase`, `camelCase`, `snake_case`,
`SCREAMING_SNAKE_CASE`, `kebab-case`, `SCREAMING-KEBAB-CASE`. On enums it renames
variants, not their fields; for fields of struct variants there is
`rename_all_fields` on the enum. Single field: `#[serde(rename = "type")]`. For different
names in each direction: `rename(serialize = "..", deserialize = "..")`. Old spellings
accepted on input: `#[serde(alias = "jobID")]`.

## deny_unknown_fields

By default unknown keys are silently ignored. For config files that is dangerous: a typo
like `wokers = 4` just does nothing. `#[serde(deny_unknown_fields)]` on the container makes
deserialization fail with `unknown field `wokers`, expected one of ...`.

Caveat: it **does not combine with `flatten`** — the flattened struct collects keys through
an internal map and serde cannot tell which ones are unknown. Using both compiles but the
behaviour is not what you want. Pick one.

## flatten

Inlines the fields of a nested struct into the parent, or catches the remaining keys:

```rust
#[derive(Deserialize)]
struct Request {
    id: u64,
    #[serde(flatten)]
    paging: Paging,                    // {"id":1,"limit":10,"offset":0}
    #[serde(flatten)]
    extra: HashMap<String, Value>,     // everything not matched above
}
```

Costs: flatten buffers the input into an intermediate representation, so it is slower, and
it does not work with formats that are not self-describing (bincode, postcard). Error
messages also get worse.

## Others in the same family

- `#[serde(default)]` — missing field uses `Default`; on the struct it applies to all.
- `#[serde(skip_serializing_if = "Option::is_none")]`.
- `#[serde(tag = "type")]` (internally tagged enum), `untagged` (tries variants in order;
  error messages are useless, so keep it for small enums).
- `#[serde(with = "humantime_serde")]` for custom field formats.
