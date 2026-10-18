//! Опции comrak, общие для разбора и рендера (ТЗ §5.3).

use comrak::Options;

/// Префикс `id` заголовков в HTML: `id = HEADER_ID_PREFIX + Heading::anchor`.
pub const HEADER_ID_PREFIX: &str = "h-";

/// Разделитель frontmatter.
pub const FRONT_MATTER_DELIMITER: &str = "---";

/// Опции comrak: GFM, сноски, вики-ссылки, alerts, формулы, frontmatter,
/// `id` заголовков и `data-sourcepos`. Сырой HTML не пропускается
/// (`render.unsafe = false`), санитизация — отдельным проходом.
pub fn comrak_options() -> Options<'static> {
    let mut o = Options::default();
    let ext = &mut o.extension;
    // GFM
    ext.strikethrough = true;
    ext.table = true;
    ext.autolink = true;
    ext.tasklist = true;
    // расширения §5.3
    ext.footnotes = true;
    ext.wikilinks_title_after_pipe = true;
    ext.alerts = true;
    ext.math_dollars = true;
    ext.math_code = true;
    ext.front_matter_delimiter = Some(FRONT_MATTER_DELIMITER.to_string());
    ext.header_id_prefix = Some(HEADER_ID_PREFIX.to_string());
    o.render.sourcepos = true;
    // только вывод: классы `task-list-item` у пунктов задач, на разбор не влияет
    o.render.tasklist_classes = true;
    o.render.r#unsafe = false;
    o
}
