//! Детерминированный текст корпуса документов: ГПСЧ SplitMix64 с независимыми потоками на
//! документ ([`Rng::fork`]) и наполнитель по шаблонам (ru, en, zh, ja) из области «эксплуатация
//! предприятия» — трудные соседи фактов: те же слова «график», «склад», «проверка», но не факты.

/// SplitMix64: без зависимостей, одинаков на всех платформах.
#[derive(Debug, Clone)]
pub struct Rng(u64);

/// FNV-1a 64: метка потока → добавка к сиду.
fn fnv64(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl Rng {
    #[cfg(test)]
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    /// Независимый поток для документа `tag`: правка одного документа не сдвигает остальные.
    pub fn fork(seed: u64, tag: &str) -> Rng {
        let mut r = Rng(seed ^ fnv64(tag));
        r.next_u64();
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Равномерно в `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Равномерно в `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.below((hi - lo + 1) as usize) as u32
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// Язык текста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
    Zh,
    Ja,
}

const EN_SUBJ: &[&str] = &[
    "The maintenance crew",
    "The night shift",
    "The logistics team",
    "The quality group",
    "The site manager",
    "The field engineers",
    "The procurement office",
    "The safety committee",
    "The control room",
    "The planning department",
];
const EN_VERB: &[&str] = &[
    "reviewed",
    "inspected",
    "updated",
    "documented",
    "verified",
    "rescheduled",
    "audited",
    "compared",
    "summarised",
    "archived",
];
const EN_OBJ: &[&str] = &[
    "the weekly schedule",
    "the spare parts list",
    "the shift handover notes",
    "the supplier contracts",
    "the calibration records",
    "the incident log",
    "the training plan",
    "the energy readings",
    "the warehouse layout",
    "the permit register",
];
const EN_TAIL: &[&str] = &[
    "before the end of the quarter",
    "after the morning briefing",
    "at the request of the regional office",
    "without major findings",
    "with two minor remarks",
    "for the second time this month",
    "using the new checklist",
    "in cooperation with the vendor",
    "as part of the annual review",
    "ahead of the holiday season",
];
const EN_PLACE: &[&str] = &[
    "the north yard",
    "building 4",
    "the river pumping station",
    "the central depot",
    "the east substation",
    "the packaging line",
    "the main gate",
    "the test laboratory",
];
const EN_UNIT: &[&str] = &[
    "work orders",
    "pallets",
    "inspections",
    "alarms",
    "deliveries",
    "visitors",
    "service requests",
    "shipments",
];

const RU_SUBJ: &[&str] = &[
    "Ремонтная бригада",
    "Ночная смена",
    "Отдел логистики",
    "Служба качества",
    "Начальник участка",
    "Выездная группа",
    "Отдел закупок",
    "Комиссия по охране труда",
    "Диспетчерская служба",
    "Плановый отдел",
];
const RU_VERB: &[&str] = &[
    "проверяет",
    "обновляет",
    "согласует",
    "документирует",
    "пересматривает",
    "сверяет",
    "архивирует",
    "уточняет",
    "анализирует",
    "утверждает",
];
const RU_OBJ: &[&str] = &[
    "недельный график",
    "список запасных частей",
    "журнал передачи смены",
    "договоры с поставщиками",
    "протоколы поверки",
    "журнал инцидентов",
    "план обучения",
    "показания счётчиков энергии",
    "схему склада",
    "реестр нарядов-допусков",
];
const RU_TAIL: &[&str] = &[
    "до конца квартала",
    "после утренней планёрки",
    "по запросу регионального офиса",
    "без существенных замечаний",
    "с двумя незначительными замечаниями",
    "второй раз за месяц",
    "по новому чек-листу",
    "совместно с подрядчиком",
    "в рамках ежегодной проверки",
    "перед праздничным периодом",
];
const RU_PLACE: &[&str] = &[
    "северного двора",
    "корпуса 4",
    "речной насосной станции",
    "центрального склада",
    "восточной подстанции",
    "упаковочной линии",
    "главной проходной",
    "испытательной лаборатории",
];
const RU_UNIT: &[&str] = &[
    "заявок",
    "поддонов",
    "осмотров",
    "срабатываний сигнализации",
    "поставок",
    "посетителей",
    "обращений",
    "отгрузок",
];

const ZH_SUBJ: &[&str] = &[
    "维修班组",
    "夜班",
    "物流部",
    "质检组",
    "现场经理",
    "工程小组",
    "采购部",
    "安全委员会",
    "调度中心",
    "计划科",
];
const ZH_VERB: &[&str] = &[
    "审核了",
    "检查了",
    "更新了",
    "记录了",
    "核对了",
    "调整了",
    "整理了",
    "比较了",
    "汇总了",
    "归档了",
];
const ZH_OBJ: &[&str] = &[
    "每周计划",
    "备件清单",
    "交接班记录",
    "供应商合同",
    "校准记录",
    "事故日志",
    "培训计划",
    "能耗数据",
    "仓库布局",
    "作业许可登记",
];
const ZH_TAIL: &[&str] = &[
    "在季度结束之前",
    "在早会之后",
    "应地区办公室的要求",
    "按照新的检查表",
    "与供应商一起",
    "作为年度审查的一部分",
    "在节假日之前",
    "本月第二次",
];

const JA_SUBJ: &[&str] = &["保守チーム", "夜勤班", "物流部", "品質管理課", "現場責任者"];
const JA_OBJ: &[&str] = &["週間計画", "部品リスト", "引継ぎ記録", "点検記録", "在庫表"];
const JA_VERB: &[&str] = &[
    "確認しました",
    "更新しました",
    "記録しました",
    "整理しました",
];

/// Предложение наполнителя.
pub fn sentence(r: &mut Rng, lang: Lang) -> String {
    match lang {
        Lang::En => {
            if r.below(4) == 0 {
                let n = r.range(5, 900);
                format!(
                    "{n} {} were recorded at {} {}.",
                    r.pick(EN_UNIT),
                    r.pick(EN_PLACE),
                    r.pick(EN_TAIL)
                )
            } else {
                format!(
                    "{} {} {} {}.",
                    r.pick(EN_SUBJ),
                    r.pick(EN_VERB),
                    r.pick(EN_OBJ),
                    r.pick(EN_TAIL)
                )
            }
        }
        Lang::Ru => {
            if r.below(4) == 0 {
                let n = r.range(5, 900);
                format!(
                    "На площадке {} зарегистрировано {n} {} {}.",
                    r.pick(RU_PLACE),
                    r.pick(RU_UNIT),
                    r.pick(RU_TAIL)
                )
            } else {
                format!(
                    "{} {} {} {}.",
                    r.pick(RU_SUBJ),
                    r.pick(RU_VERB),
                    r.pick(RU_OBJ),
                    r.pick(RU_TAIL)
                )
            }
        }
        Lang::Zh => format!(
            "{}{}{}{}。",
            r.pick(ZH_SUBJ),
            r.pick(ZH_TAIL),
            r.pick(ZH_VERB),
            r.pick(ZH_OBJ)
        ),
        Lang::Ja => format!(
            "{}は{}を{}。",
            r.pick(JA_SUBJ),
            r.pick(JA_OBJ),
            r.pick(JA_VERB)
        ),
    }
}

/// Абзац из `lo..=hi` предложений.
pub fn paragraph(r: &mut Rng, lang: Lang, lo: u32, hi: u32) -> String {
    let n = r.range(lo, hi);
    let sep = if matches!(lang, Lang::Zh | Lang::Ja) {
        ""
    } else {
        " "
    };
    (0..n)
        .map(|_| sentence(r, lang))
        .collect::<Vec<_>>()
        .join(sep)
}

/// Строки текста шириной не больше `max` (по оценке `width`): по словам, у CJK — по символам.
pub fn wrap(text: &str, max: f32, width: impl Fn(&str) -> f32) -> Vec<String> {
    let cjk = text.chars().any(is_wide);
    let mut out = Vec::new();
    let mut cur = String::new();
    if cjk {
        for c in text.chars() {
            let mut next = cur.clone();
            next.push(c);
            if width(&next) > max && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                cur.push(c);
            } else {
                cur = next;
            }
        }
    } else {
        for w in text.split_whitespace() {
            let next = if cur.is_empty() {
                w.to_string()
            } else {
                format!("{cur} {w}")
            };
            if width(&next) > max && !cur.is_empty() {
                out.push(std::mem::replace(&mut cur, w.to_string()));
            } else {
                cur = next;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Широкий (CJK) символ: ширина 1 em.
pub fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_streams_are_stable_and_independent() {
        let a: Vec<u64> = (0..4).map(|_| Rng::fork(7, "a").next_u64()).collect();
        assert!(a.windows(2).all(|w| w[0] == w[1]));
        assert_ne!(Rng::fork(7, "a").next_u64(), Rng::fork(7, "b").next_u64());
        // эталон SplitMix64 (seed 0, первое значение)
        assert_eq!(Rng::new(0).next_u64(), 0xe220_a839_7b1d_cdaf);
    }

    #[test]
    fn wrap_respects_width() {
        let w = |s: &str| s.chars().count() as f32;
        let lines = wrap("aa bb cc dd ee", 5.0, w);
        assert_eq!(lines, ["aa bb", "cc dd", "ee"]);
        let cjk = wrap("仓库温度必须保持", 3.0, w);
        assert_eq!(cjk, ["仓库温", "度必须", "保持"]);
    }
}
