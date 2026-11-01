//! Рабочий процесс `zam-server doc-worker` (M5 §2): разбор документов (прежде всего PDF через
//! pdfium) по кадрам stdin/stdout ([`super::protocol`]), без tokio и без конфигурации сервера.
//!
//! - журнал — свой, JSON в stderr (родитель пересылает строки в свой журнал); содержимое
//!   документов не журналируется;
//! - pdfium загружается лениво, при первом PDF, миниатюре или `Ping`; ошибка загрузки — ответ
//!   `unsupported` с причиной;
//! - паника разбора — ответ `internal` и выход процесса: состояние pdfium после неё не
//!   доверяется, следующий запрос получит новый процесс.

use std::io::{self, BufReader, BufWriter};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use zam_docs::pdf::Pdfium;
use zam_docs::{DocError, DocKind, Limits};

use super::protocol::{
    Request, Response, WireError, WireExtracted, parse_kind, read_frame, write_frame,
};

/// Коды выхода рабочего процесса.
const EXIT_FRAME: i32 = 2;
const EXIT_WRITE: i32 = 3;
const EXIT_PANIC: i32 = 4;

/// Состояние процесса: путь pdfium и лениво загруженная библиотека.
struct State {
    pdfium_path: PathBuf,
    lib: Option<Result<Pdfium, String>>,
}

impl State {
    fn pdfium(&mut self) -> Result<&Pdfium, String> {
        let path = &self.pdfium_path;
        self.lib
            .get_or_insert_with(|| load_pdfium(path))
            .as_ref()
            .map_err(Clone::clone)
    }
}

fn load_pdfium(path: &Path) -> Result<Pdfium, String> {
    if !path.is_file() {
        return Err(format!("pdfium не найден: {}", path.display()));
    }
    zam_docs::pdf::bind(path).map_err(|e| format!("pdfium не загружен ({}): {e}", path.display()))
}

/// Журнал процесса: JSON в stderr (формат разбирает `zam_index::child::parse_json_line`).
fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init();
}

/// Точка входа подкоманды: читает кадры до EOF; возвращает код выхода.
pub fn run(pdfium: PathBuf, max_frame: usize) -> i32 {
    init_logging();
    tracing::info!(
        pid = std::process::id(),
        "рабочий процесс разбора документов запущен"
    );
    let mut st = State {
        pdfium_path: pdfium,
        lib: None,
    };
    let mut input = BufReader::with_capacity(64 * 1024, io::stdin().lock());
    let mut output = BufWriter::with_capacity(64 * 1024, io::stdout().lock());
    loop {
        let (id, body) = match read_frame(&mut input, max_frame) {
            Ok(Some(f)) => f,
            Ok(None) => return 0,
            Err(e) => {
                tracing::error!("кадр запроса: {e}");
                return EXIT_FRAME;
            }
        };
        let (resp, panicked) = match catch_unwind(AssertUnwindSafe(|| handle(&body, &mut st))) {
            Ok(r) => (r, false),
            Err(_) => (
                Reply::One(error(DocError::Internal("паника разбора".into()))),
                true,
            ),
        };
        #[cfg(feature = "test-hooks")]
        let (resp, spam) = match resp {
            Reply::One(r) => (r, None),
            Reply::Spam(frames, mb) => (Response::Done, Some((frames, mb))),
        };
        #[cfg(not(feature = "test-hooks"))]
        let Reply::One(resp) = resp;
        drop(body);
        let out = serde_json::to_vec(&resp).unwrap_or_else(|e| {
            serde_json::to_vec(&error(DocError::Internal(format!("ответ: {e}"))))
                .unwrap_or_default()
        });
        drop(resp);
        if let Err(e) = write_frame(&mut output, id, &out) {
            tracing::error!("кадр ответа: {e}");
            return EXIT_WRITE;
        }
        #[cfg(feature = "test-hooks")]
        if let Some((frames, mb)) = spam {
            // лишние кадры без запроса: пул не должен их копить и путать с ответами
            let pad = "x".repeat(usize::try_from(mb * 1024 * 1024).unwrap_or(0));
            for i in 0..frames {
                let r = Response::Pong {
                    pdfium: Some(format!("stale-{i}{pad}")),
                };
                let b = serde_json::to_vec(&r).unwrap_or_default();
                if write_frame(&mut output, id, &b).is_err() {
                    return EXIT_WRITE;
                }
            }
        }
        if panicked {
            tracing::error!("паника разбора — процесс завершается");
            return EXIT_PANIC;
        }
    }
}

/// Ответ на запрос; `Spam` — тестовый поток лишних кадров после ответа.
enum Reply {
    One(Response),
    #[cfg(feature = "test-hooks")]
    Spam(u32, u64),
}

fn error(e: DocError) -> Response {
    Response::Error(WireError::from(&e))
}

fn decode(b64: &str) -> Result<Vec<u8>, DocError> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| DocError::Internal(format!("base64: {e}")))
}

fn handle(body: &[u8], st: &mut State) -> Reply {
    #[cfg(feature = "test-hooks")]
    if let Ok(Request::Spam { frames, mb }) = serde_json::from_slice::<Request<'_>>(body) {
        return Reply::Spam(frames, mb);
    }
    Reply::One(respond(body, st))
}

fn respond(body: &[u8], st: &mut State) -> Response {
    let req: Request<'_> = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return error(DocError::Internal(format!("запрос: {e}"))),
    };
    match req {
        Request::Ping => Response::Pong {
            pdfium: st.pdfium().err(),
        },
        Request::Extract {
            kind,
            limits,
            soft_ms,
            bytes,
        } => {
            let Some(kind) = parse_kind(&kind) else {
                return error(DocError::Unsupported);
            };
            let limits = Limits::from(&limits);
            let bytes = match decode(&bytes) {
                Ok(b) => b,
                Err(e) => return error(e),
            };
            let deadline = Instant::now() + Duration::from_millis(soft_ms);
            let r = if kind == DocKind::Pdf {
                match st.pdfium() {
                    Ok(lib) => zam_docs::pdf::extract_pdf(lib, &bytes, &limits, deadline),
                    Err(reason) => {
                        return Response::Error(WireError {
                            kind: "unsupported".into(),
                            message: reason,
                        });
                    }
                }
            } else {
                zam_docs::extract(kind, &bytes, &limits, deadline)
            };
            match r {
                Ok(x) => Response::Extracted(WireExtracted::from(x)),
                Err(e) => error(e),
            }
        }
        Request::Thumb {
            page,
            max_px,
            bytes,
        } => {
            let bytes = match decode(&bytes) {
                Ok(b) => b,
                Err(e) => return error(e),
            };
            let lib = match st.pdfium() {
                Ok(l) => l,
                Err(reason) => {
                    return Response::Error(WireError {
                        kind: "unsupported".into(),
                        message: reason,
                    });
                }
            };
            match zam_docs::pdf::render_thumb(lib, &bytes, page, max_px) {
                Ok(png) => Response::Thumb {
                    png: base64::engine::general_purpose::STANDARD.encode(png),
                },
                Err(e) => error(e),
            }
        }
        #[cfg(feature = "test-hooks")]
        Request::Crash => std::process::abort(),
        #[cfg(feature = "test-hooks")]
        Request::Sleep { ms } => {
            std::thread::sleep(Duration::from_millis(ms));
            Response::Done
        }
        #[cfg(feature = "test-hooks")]
        Request::Alloc { mb } => {
            // по 16 МиБ с записью в каждую страницу: память действительно выделяется
            let mut keep: Vec<Vec<u8>> = Vec::new();
            for _ in 0..mb.div_ceil(16) {
                keep.push(vec![1u8; 16 * 1024 * 1024]);
            }
            std::hint::black_box(&keep);
            Response::Done
        }
        #[cfg(feature = "test-hooks")]
        Request::Spam { .. } => Response::Done,
        #[cfg(feature = "test-hooks")]
        Request::LongError { kb } => Response::Error(WireError {
            kind: "unsupported".into(),
            message: "x".repeat(usize::try_from(kb * 1024).unwrap_or(0)),
        }),
    }
}
