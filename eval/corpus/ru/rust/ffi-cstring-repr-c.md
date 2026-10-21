---
tags: [rust, ffi, unsafe]
---
# FFI с C: CString, CStr и #[repr(C)]

Заметки после обёртки C-библиотеки для разбора логов.

## Строки

В Rust `String` — UTF-8 без завершающего нуля, в C `char*` — байты до `\0`.

- **Rust → C**: `CString::new(s)?` добавляет `\0`. Возвращает ошибку `NulError`, если внутри
  строки уже есть нулевой байт — это надо обработать, а не `unwrap()`.
- **C → Rust**: `unsafe { CStr::from_ptr(p) }` и дальше `.to_str()?` (проверка UTF-8) или
  `.to_string_lossy()`.
- Литералы: `c"hello"` даёт `&CStr` (стабильно с 1.77).

Самая коварная ошибка:

```rust
let p = CString::new(name)?.as_ptr();   // CString уничтожен в конце оператора
unsafe { lib_open(p) };                 // висячий указатель
```

Компилятор выдаёт предупреждение `dangling_pointers_from_temporaries`, но код собирается.
Правильно — сохранить `CString` в переменной, пока C-функция работает с указателем:

```rust
let c_name = CString::new(name)?;
let rc = unsafe { lib_open(c_name.as_ptr()) };
```

Если C-код забирает строку себе, отдаём владение через `into_raw()` и обязательно
возвращаем его обратно `CString::from_raw()` для освобождения. Освобождать память,
выделенную Rust, через `free()` из C нельзя — разные аллокаторы.

## Структуры

Порядок и выравнивание полей Rust-структуры не гарантированы. Для передачи в C:

```rust
#[repr(C)]
pub struct LogEntry {
    pub ts: u64,
    pub level: u32,
    pub msg: *const c_char,
}
```

Для перечислений — `#[repr(C)]` или `#[repr(u32)]`; получать из C значение enum, которого
нет среди вариантов, — неопределённое поведение, поэтому на границе лучше принимать
целое число и проверять его.

## Объявления

```rust
unsafe extern "C" {
    fn lib_open(name: *const c_char) -> c_int;
}

#[unsafe(no_mangle)]
pub extern "C" fn rs_callback(entry: *const LogEntry) { /* ... */ }
```

С редакции 2024 блок `extern` обязан быть `unsafe extern`, а `#[no_mangle]` пишется как
`#[unsafe(no_mangle)]`.

## Паника на границе

Раскрутка паники через `extern "C"` функцию приводит к аварийному завершению процесса.
В колбэках, вызываемых из C, оборачиваем тело в `std::panic::catch_unwind` и возвращаем
код ошибки. Если раскрутка через границу нужна намеренно — ABI `"C-unwind"`.

## Инструменты

`bindgen` генерирует Rust-объявления из заголовков C, `cbindgen` — наоборот, заголовок для
Rust-библиотеки. Для проверки памяти — `cargo miri test` для чистого Rust и
AddressSanitizer (`-Zsanitizer=address` на nightly) для смешанного кода.
