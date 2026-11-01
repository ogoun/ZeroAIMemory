//! Составной документ OLE (CFB, MS-CFB): чтение потока по имени — только для XLS.
//!
//! Защита: каждая цепочка секторов (FAT, DIFAT, мини-FAT, каталог, поток) проходится не дальше
//! числа секторов файла (цикл — `Malformed`), номера секторов — в пределах файла, размер
//! потока — не больше длины его цепочки и `max_unpacked`. Дерево каталога обходится с
//! отметкой посещённых записей.

use crate::DocError;
use crate::ctx::Ctx;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const END: u32 = 0xFFFF_FFFE;
const FREE: u32 = 0xFFFF_FFFF;
const NOSTREAM: u32 = 0xFFFF_FFFF;
const MINI: usize = 64;

fn bad(why: &str) -> DocError {
    DocError::Malformed(format!("cfb: {why}"))
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Запись каталога: имя, тип (1 — хранилище, 2 — поток, 5 — корень), дерево, данные.
struct Entry {
    name: String,
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

pub(crate) struct Cfb<'a> {
    b: &'a [u8],
    shift: u32,
    sectors: u32,
    fat: Vec<u32>,
    minifat: Vec<u32>,
    /// Секторы мини-потока (цепочка корневой записи).
    mini: Vec<u32>,
    cutoff: u64,
    dir: Vec<Entry>,
}

impl<'a> Cfb<'a> {
    /// Открывает файл; записей каталога — не больше `max_entries` (остальные не читаются).
    pub fn open(b: &'a [u8], ctx: &mut Ctx) -> Result<Self, DocError> {
        if b.len() < 512 || b[..8] != SIGNATURE {
            return Err(bad("нет заголовка"));
        }
        let shift = u32::from(u16_at(b, 0x1E));
        if shift != 9 && shift != 12 {
            return Err(bad("размер сектора"));
        }
        let ssz = 1usize << shift;
        if b.len() < ssz {
            return Err(bad("короче сектора"));
        }
        let sectors = u32::try_from((b.len() - ssz) / ssz).map_err(|_| bad("размер"))?;
        let mut cfb = Cfb {
            b,
            shift,
            sectors,
            fat: Vec::new(),
            minifat: Vec::new(),
            mini: Vec::new(),
            cutoff: u64::from(u32_at(b, 0x38)),
            dir: Vec::new(),
        };
        // MS-CFB: порог мини-потока — ровно 4096; иной позволил бы «мини» поток любого размера
        if cfb.cutoff != 4096 {
            return Err(bad("порог мини-потока не 4096"));
        }
        // секторы FAT: 109 в заголовке, остальные — по цепочке DIFAT
        let mut fat_sectors: Vec<u32> = (0..109)
            .map(|i| u32_at(b, 0x4C + i * 4))
            .filter(|&s| s < sectors)
            .collect();
        let mut difat = u32_at(b, 0x44);
        let mut seen = Visited::new(sectors);
        while difat < sectors {
            if !seen.insert(difat) {
                return Err(bad("цикл DIFAT"));
            }
            let s = cfb.sector(difat)?;
            let n = ssz / 4 - 1;
            fat_sectors.extend((0..n).map(|i| u32_at(s, i * 4)).filter(|&x| x < sectors));
            if fat_sectors.len() as u64 > u64::from(sectors) {
                return Err(bad("секторов FAT больше, чем в файле"));
            }
            difat = u32_at(s, n * 4);
        }
        let mut fat = Vec::with_capacity(fat_sectors.len() * ssz / 4);
        for s in fat_sectors {
            let s = cfb.sector(s)?;
            fat.extend((0..ssz / 4).map(|i| u32_at(s, i * 4)));
        }
        cfb.fat = fat;
        // каталог
        let dir_chain = cfb.chain(u32_at(b, 0x30), false)?;
        let max = ctx.limits.max_entries as usize;
        let mut dir = Vec::with_capacity((dir_chain.len() * ssz / 128).min(max));
        for s in dir_chain {
            if dir.len() >= max {
                ctx.truncated("max_entries");
                break;
            }
            let s = cfb.sector(s)?;
            for e in s.as_chunks::<128>().0 {
                let len = usize::from(u16_at(e, 0x40)).min(64);
                let units: Vec<u16> = (0..len / 2)
                    .map(|i| u16_at(e, i * 2))
                    .take_while(|&u| u != 0)
                    .collect();
                let size = if shift == 9 {
                    u64::from(u32_at(e, 0x78))
                } else {
                    u64::from(u32_at(e, 0x78)) | (u64::from(u32_at(e, 0x7C)) << 32)
                };
                dir.push(Entry {
                    name: String::from_utf16_lossy(&units),
                    kind: e[0x42],
                    left: u32_at(e, 0x44),
                    right: u32_at(e, 0x48),
                    child: u32_at(e, 0x4C),
                    start: u32_at(e, 0x74),
                    size,
                });
            }
        }
        if dir.first().is_none_or(|r| r.kind != 5) {
            return Err(bad("нет корневой записи"));
        }
        cfb.dir = dir;
        // мини-FAT и мини-поток
        let mut minifat = Vec::new();
        for s in cfb.chain(u32_at(b, 0x3C), true)? {
            let s = cfb.sector(s)?;
            minifat.extend((0..ssz / 4).map(|i| u32_at(s, i * 4)));
        }
        cfb.minifat = minifat;
        cfb.mini = cfb.chain(cfb.dir[0].start, true)?;
        Ok(cfb)
    }

    fn sector(&self, id: u32) -> Result<&'a [u8], DocError> {
        let ssz = 1usize << self.shift;
        let at = (id as usize + 1) << self.shift;
        self.b
            .get(at..at + ssz)
            .ok_or_else(|| bad("сектор за концом файла"))
    }

    /// Цепочка секторов от `start`; `empty_ok` — допустим пустой конец (нет мини-FAT).
    fn chain(&self, start: u32, empty_ok: bool) -> Result<Vec<u32>, DocError> {
        let mut out = Vec::new();
        let mut s = start;
        if empty_ok && (s == END || s == FREE) {
            return Ok(out);
        }
        let mut seen = Visited::new(self.sectors);
        while s != END {
            if s >= self.sectors {
                return Err(bad("цепочка секторов"));
            }
            if !seen.insert(s) {
                return Err(bad("цикл в цепочке секторов"));
            }
            out.push(s);
            s = *self.fat.get(s as usize).ok_or_else(|| bad("FAT"))?;
        }
        Ok(out)
    }

    /// Поток верхнего уровня (ребёнок корня) по имени без учёта регистра ASCII.
    pub fn find(&self, names: &[&str]) -> Option<usize> {
        let mut seen = vec![false; self.dir.len()];
        let mut stack = vec![self.dir[0].child];
        while let Some(i) = stack.pop() {
            if i == NOSTREAM {
                continue;
            }
            let i = i as usize;
            if i >= self.dir.len() || seen[i] {
                continue;
            }
            seen[i] = true;
            let e = &self.dir[i];
            if e.kind == 2 && names.iter().any(|n| n.eq_ignore_ascii_case(&e.name)) {
                return Some(i);
            }
            stack.push(e.left);
            stack.push(e.right);
        }
        None
    }

    /// Поток записи `i` целиком в пределах `max_unpacked` (сверх — обрезка с предупреждением).
    pub fn read(&self, i: usize, ctx: &mut Ctx) -> Result<Vec<u8>, DocError> {
        let e = &self.dir[i];
        let mut size = e.size;
        if size > ctx.limits.max_unpacked {
            size = ctx.limits.max_unpacked;
            ctx.truncated("max_unpacked");
        }
        let size = usize::try_from(size).map_err(|_| bad("размер потока"))?;
        if e.size < self.cutoff {
            // мини-поток: не больше ёмкости мини-потока; шагов не больше мини-секторов, цикл —
            // ошибка (отметка посещённых)
            let capacity = self.mini.len() << self.shift;
            if size > capacity {
                return Err(bad("размер мини-потока"));
            }
            let mini_sectors = u32::try_from((capacity / MINI).min(self.minifat.len()))
                .map_err(|_| bad("мини-FAT"))?;
            let mut seen = Visited::new(mini_sectors);
            let mut out = Vec::with_capacity(size);
            let mut s = e.start;
            while out.len() < size && s != END {
                if s >= mini_sectors || !seen.insert(s) {
                    return Err(bad("цепочка мини-секторов"));
                }
                let at = s as usize * MINI;
                let sec = *self
                    .mini
                    .get(at >> self.shift)
                    .ok_or_else(|| bad("мини-сектор"))?;
                let off = at & ((1usize << self.shift) - 1);
                let data = self.sector(sec)?;
                let take = (size - out.len()).min(MINI).min(data.len() - off);
                out.extend_from_slice(&data[off..off + take]);
                s = *self
                    .minifat
                    .get(s as usize)
                    .ok_or_else(|| bad("мини-FAT"))?;
            }
            Ok(out)
        } else {
            // размер — не больше цепочки: заявленному не верим
            let chain = self.chain(e.start, true)?;
            let size = size.min(chain.len() << self.shift);
            let mut out = Vec::with_capacity(size);
            for s in chain {
                if out.len() >= size {
                    break;
                }
                let data = self.sector(s)?;
                let take = (size - out.len()).min(data.len());
                out.extend_from_slice(&data[..take]);
            }
            Ok(out)
        }
    }
}

/// Отметки посещённых секторов цепочки (бит на сектор): повтор — цикл.
struct Visited(Vec<u64>);

impl Visited {
    fn new(n: u32) -> Self {
        Visited(vec![0; (n as usize).div_ceil(64)])
    }

    /// `false` — сектор уже был (или вне карты).
    fn insert(&mut self, s: u32) -> bool {
        let (w, bit) = ((s / 64) as usize, 1u64 << (s % 64));
        match self.0.get_mut(w) {
            Some(x) if *x & bit == 0 => {
                *x |= bit;
                true
            }
            _ => false,
        }
    }
}
