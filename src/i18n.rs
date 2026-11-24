//! UI language: every UI text is written in English in the code and looked up here in the chosen
//! language (English when a text has no translation), the choice is kept in a settings file, and the
//! Korean, Japanese and Chinese glyphs come from fonts of the operating system, read when first needed.
use eframe::egui;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Lang {
    En,
    Ko,
    Ja,
    Zh,
    Es,
    De,
    Fr,
}

/// View > Language: each language with its code and its name in its own script
pub const LANGS: [(Lang, &str, &str); 7] = [
    (Lang::En, "en", "English"),
    (Lang::Ko, "ko", "한국어"),
    (Lang::Ja, "ja", "日本語"),
    (Lang::Zh, "zh", "简体中文"),
    (Lang::Es, "es", "Español"),
    (Lang::De, "de", "Deutsch"),
    (Lang::Fr, "fr", "Français"),
];

impl Lang {
    pub fn code(self) -> &'static str {
        LANGS[self as usize].1
    }
    pub fn name(self) -> &'static str {
        LANGS[self as usize].2
    }
    pub fn from_code(s: &str) -> Option<Lang> {
        let s = s.trim().to_ascii_lowercase();
        LANGS.iter().find(|l| l.1 == s).map(|l| l.0)
    }
    /// English name, for notes that must stay readable when the language's own glyphs are missing
    fn english(self) -> &'static str {
        ["English", "Korean", "Japanese", "Chinese", "Spanish", "German", "French"][self as usize]
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    LANGS[(LANG.load(Ordering::Relaxed) as usize).min(LANGS.len() - 1)].0
}
pub fn is_en() -> bool {
    lang() == Lang::En
}
pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

/// the UI text `en` in the current language, or `en` itself when it has no translation
pub fn tr(en: &str) -> &str {
    let l = lang();
    if l == Lang::En {
        return en;
    }
    match index().get(en) {
        Some(&i) if !T[i][l as usize].is_empty() => T[i][l as usize],
        _ => en,
    }
}

fn index() -> &'static HashMap<&'static str, usize> {
    static IX: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    IX.get_or_init(|| T.iter().enumerate().map(|(i, r)| (r[0], i)).collect())
}

/// `format!` for UI texts: the English template is translated, then filled. In English it is exactly
/// `format!`, which also checks the template against the arguments at compile time.
#[macro_export]
macro_rules! trf {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_en() {
            format!($fmt $(, $arg)*)
        } else {
            $crate::i18n::fill($crate::i18n::tr($fmt), &[$(&$arg as &dyn ::std::fmt::Display),*])
        }
    };
}

/// fill a template: `{}` takes the next argument, `{2}` the third, both with an optional `:[<>^][width][.precision]`.
/// A placeholder without its argument stays as written, so a bad translation shows instead of hiding.
pub fn fill(t: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(t.len() + 16);
    let mut next = 0;
    let mut i = 0;
    while i < t.len() {
        let rest = &t[i..];
        if rest.starts_with("{{") || rest.starts_with("}}") {
            out.push_str(&rest[..1]);
            i += 2;
            continue;
        }
        if rest.starts_with('{') {
            if let Some(end) = rest.find('}') {
                let inner = &rest[1..end];
                let (pos, spec) = inner.split_once(':').unwrap_or((inner, ""));
                let k = if pos.is_empty() {
                    next += 1;
                    next - 1
                } else {
                    pos.parse().unwrap_or(usize::MAX)
                };
                match args.get(k) {
                    Some(a) => out.push_str(&fmt_arg(*a, spec)),
                    None => out.push_str(&rest[..=end]),
                }
                i += end + 1;
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// one argument with a format spec (numbers need an explicit `>` to be right aligned)
fn fmt_arg(a: &dyn Display, spec: &str) -> String {
    let (align, rest) = match spec.as_bytes().first() {
        Some(b'<') => ('<', &spec[1..]),
        Some(b'>') => ('>', &spec[1..]),
        Some(b'^') => ('^', &spec[1..]),
        _ => ('<', spec),
    };
    let (w, p) = rest.split_once('.').map_or((rest, None), |(w, p)| (w, Some(p)));
    let s = match p.and_then(|p| p.parse::<usize>().ok()) {
        Some(p) => format!("{:.*}", p, a),
        None => a.to_string(),
    };
    let width = w.parse::<usize>().unwrap_or(0);
    let n = s.chars().count();
    if n >= width {
        return s;
    }
    let pad = width - n;
    match align {
        '>' => " ".repeat(pad) + &s,
        '^' => " ".repeat(pad / 2) + &s + &" ".repeat(pad - pad / 2),
        _ => s + &" ".repeat(pad),
    }
}

/// true when `s` was made from the template `en`, in English or in the current language
/// (compares the text before the first placeholder)
pub fn made_from(s: &str, en: &str) -> bool {
    let head = |t: &str| t.split('{').next().unwrap_or("").to_string();
    [head(en), head(tr(en))].iter().any(|h| !h.is_empty() && s.starts_with(h.as_str()))
}

// ---------- the stored choice ----------
/// settings file: Windows %APPDATA%\lsv, macOS ~/Library/Application Support/lsv, else $XDG_CONFIG_HOME/lsv or ~/.config/lsv
pub fn settings_path() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    };
    dir.map(|d| d.join("lsv").join("settings.txt"))
}

/// value of `key` in key=value lines
fn get_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim()))
}

/// the same lines with `key` set to `value` (added when missing)
fn set_key(text: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| match l.split_once('=') {
            Some((k, _)) if k.trim() == key => {
                found = true;
                format!("{}={}", key, value)
            }
            _ => l.to_string(),
        })
        .collect();
    if !found {
        out.push(format!("{}={}", key, value));
    }
    out.join("\n") + "\n"
}

/// the language stored by an earlier run (none on the first run)
pub fn load_lang() -> Option<Lang> {
    let p = settings_path()?;
    let text = std::fs::read_to_string(&p).ok()?;
    let v = get_key(&text, "language")?;
    let l = Lang::from_code(v);
    if l.is_none() {
        eprintln!("lsv: {}: unknown language {:?}, using English", p.display(), v);
    }
    l
}

/// store the choice for the next runs
pub fn save_lang(l: Lang) -> Result<PathBuf, String> {
    let p = settings_path().ok_or("no settings folder (APPDATA, HOME or XDG_CONFIG_HOME is not set)")?;
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {}", d.display(), e))?;
    }
    std::fs::write(&p, set_key(&old, "language", l.code())).map_err(|e| format!("{}: {}", p.display(), e))?;
    Ok(p)
}

/// before the window opens: the language from LSV_LANG (test hook, not stored), else the stored one,
/// else English (call `install_fonts` once the egui context exists)
pub fn start() {
    let from_env = std::env::var("LSV_LANG").ok().and_then(|c| {
        let l = Lang::from_code(&c);
        if l.is_none() {
            eprintln!("lsv: LSV_LANG={}: unknown language, expected one of en ko ja zh es de fr", c);
        }
        l
    });
    set_lang(from_env.or_else(load_lang).unwrap_or(Lang::En));
    // a CJK UI: read its font while the window and the GPU start, so the first frame waits less
    let l = lang();
    if slot(l).is_some() {
        std::thread::spawn(move || {
            let _ = load(l);
        });
    }
}

// ---------- fonts ----------
/// a font file of the system for one CJK language: path, face index (None: pick by name), wanted face names
struct FontFile {
    path: PathBuf,
    index: Option<u32>,
    want: &'static [&'static str],
}

fn slot(l: Lang) -> Option<usize> {
    match l {
        Lang::Ko => Some(0),
        Lang::Ja => Some(1),
        Lang::Zh => Some(2),
        _ => None,
    }
}

/// font files to try, in order, with the PostScript names of the faces wanted in a collection
fn candidates(l: Lang) -> Vec<(PathBuf, &'static [&'static str])> {
    let mut v: Vec<(PathBuf, &'static [&'static str])> = vec![];
    if cfg!(windows) {
        let root = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        let f = root.join("Fonts");
        let names: &[(&str, &'static [&'static str])] = match l {
            Lang::Ko => &[("malgun.ttf", &[]), ("gulim.ttc", &[])],
            Lang::Ja => &[("YuGothR.ttc", &["YuGothic-Regular"]), ("YuGothM.ttc", &["YuGothic-Medium"]), ("meiryo.ttc", &["Meiryo"]), ("msgothic.ttc", &["MS-UIGothic"])],
            _ => &[("msyh.ttc", &["MicrosoftYaHeiUI", "MicrosoftYaHei"]), ("msyh.ttf", &[]), ("simsun.ttc", &[]), ("simhei.ttf", &[])],
        };
        v.extend(names.iter().map(|(n, w)| (f.join(n), *w)));
    } else if cfg!(target_os = "macos") {
        let names: &[(&str, &'static [&'static str])] = match l {
            Lang::Ko => &[("/System/Library/Fonts/AppleSDGothicNeo.ttc", &["AppleSDGothicNeo-Regular"]), ("/System/Library/Fonts/Supplemental/AppleGothic.ttf", &[])],
            Lang::Ja => &[("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", &["HiraginoSans-W3"]), ("/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc", &[])],
            _ => &[
                ("/System/Library/Fonts/PingFang.ttc", &["PingFangSC-Regular"]),
                ("/System/Library/Fonts/Hiragino Sans GB.ttc", &["HiraginoSansGB-W3"]),
                ("/System/Library/Fonts/STHeiti Light.ttc", &["STHeitiSC-Light"]),
                ("/System/Library/Fonts/Supplemental/Songti.ttc", &["STSongti-SC-Regular"]),
            ],
        };
        v.extend(names.iter().map(|(n, w)| (PathBuf::from(n), *w)));
        v.push((PathBuf::from("/Library/Fonts/Arial Unicode.ttf"), &[]));
        v.push((PathBuf::from("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"), &[]));
    } else {
        let noto: &'static [&'static str] = match l {
            Lang::Ko => &["NotoSansCJKkr"],
            Lang::Ja => &["NotoSansCJKjp"],
            _ => &["NotoSansCJKsc"],
        };
        for d in ["/usr/share/fonts/opentype/noto", "/usr/share/fonts/noto-cjk", "/usr/share/fonts/google-noto-cjk", "/usr/share/fonts/truetype/noto", "/usr/share/fonts/google-noto-sans-cjk-fonts"] {
            v.push((PathBuf::from(d).join("NotoSansCJK-Regular.ttc"), noto));
        }
        v.push((PathBuf::from("/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc"), noto));
        let more: &[&str] = match l {
            Lang::Ko => &["/usr/share/fonts/truetype/nanum/NanumGothic.ttf", "/usr/share/fonts/nanum/NanumGothic.ttf"],
            Lang::Ja => &["/usr/share/fonts/opentype/ipafont-gothic/ipagp.ttf", "/usr/share/fonts/ipa-pgothic/ipagp.ttf"],
            _ => &["/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", "/usr/share/fonts/wqy-microhei/wqy-microhei.ttc"],
        };
        v.extend(more.iter().map(|p| (PathBuf::from(p), &[] as &'static [&'static str])));
        v.push((PathBuf::from("/usr/share/fonts/google-droid-sans-fonts/DroidSansFallbackFull.ttf"), &[]));
        v.push((PathBuf::from("/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf"), &[]));
    }
    v
}

/// fontconfig's best sans font for the language, if it really covers the language
fn fc_match(l: Lang) -> Option<FontFile> {
    let tag = match l {
        Lang::Ko => "ko",
        Lang::Ja => "ja",
        _ => "zh-cn",
    };
    let out = std::process::Command::new("fc-match").arg("-f").arg("%{file}\n%{index}\n%{lang}").arg(format!("sans-serif:lang={}", tag)).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut it = text.lines();
    let (file, index, langs) = (it.next()?, it.next()?, it.next().unwrap_or(""));
    if !langs.split('|').any(|x| x == tag) {
        return None;
    }
    let path = PathBuf::from(file);
    // variable fonts carry the named instance in the upper 16 bits
    let index = index.trim().parse::<u32>().ok()? & 0xFFFF;
    path.is_file().then_some(FontFile { path, index: Some(index), want: &[] })
}

/// where the font for a CJK language is (looked up once)
fn locate(l: Lang) -> Option<&'static FontFile> {
    static FOUND: [OnceLock<Option<FontFile>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    FOUND[slot(l)?]
        .get_or_init(|| {
            // test hook: LSV_NO_CJK_FONTS=1 behaves as a computer without such fonts
            if std::env::var_os("LSV_NO_CJK_FONTS").is_some() {
                return None;
            }
            let fc = if cfg!(windows) || cfg!(target_os = "macos") { None } else { fc_match(l) };
            fc.or_else(|| candidates(l).into_iter().find(|(p, _)| p.is_file()).map(|(path, want)| FontFile { path, index: None, want }))
        })
        .as_ref()
}

/// the font of a CJK language, read on first use
fn load(l: Lang) -> Option<egui::FontData> {
    static LOADED: [OnceLock<Option<(egui::epaint::text::Blob, u32)>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let (blob, index) = LOADED[slot(l)?]
        .get_or_init(|| {
            let f = locate(l)?;
            // one file can serve several languages (Noto Sans CJK has them all): read it once
            static FILES: std::sync::Mutex<Vec<(PathBuf, egui::epaint::text::Blob)>> = std::sync::Mutex::new(Vec::new());
            let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
            let t0 = std::time::Instant::now();
            let (blob, read) = match files.iter().find(|(p, _)| *p == f.path) {
                Some((_, b)) => (b.clone(), false),
                None => match std::fs::read(&f.path) {
                    Ok(b) => {
                        let b = Arc::new(b) as egui::epaint::text::Blob;
                        files.push((f.path.clone(), b.clone()));
                        (b, true)
                    }
                    Err(e) => {
                        eprintln!("lsv: cannot read the {} font {}: {}", l.english(), f.path.display(), e);
                        return None;
                    }
                },
            };
            let bytes: &[u8] = (*blob).as_ref();
            let index = f.index.unwrap_or_else(|| pick_face(bytes, f.want));
            let how = if read { format!("{:.1} MB read in {} ms", bytes.len() as f64 / 1e6, t0.elapsed().as_millis()) } else { "file already read".to_string() };
            eprintln!("lsv: {} font {} (face {}), {}", l.english(), f.path.display(), index, how);
            Some((blob, index))
        })
        .clone()?;
    let mut d = egui::FontData::from_blob(blob, index);
    // variable CJK fonts (Noto Sans CJK VF) default to their thinnest weight
    d.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", 400.0)]);
    Some(d)
}

/// index of the first face whose PostScript name contains one of `want` (in that order), else 0
fn pick_face(b: &[u8], want: &[&str]) -> u32 {
    let names = face_names(b);
    for w in want {
        let w = w.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| n.to_ascii_lowercase().contains(&w)) {
            return i as u32;
        }
    }
    0
}

/// PostScript names (name ID 6) of the faces of a .ttf, .otf or .ttc file
fn face_names(b: &[u8]) -> Vec<String> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|x| u16::from_be_bytes([x[0], x[1]]) as usize);
    let u32_at = |o: usize| b.get(o..o + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]) as usize);
    let faces: Vec<usize> = if b.starts_with(b"ttcf") { (0..u32_at(8).unwrap_or(0).min(64)).filter_map(|i| u32_at(12 + 4 * i)).collect() } else { vec![0] };
    let name_of = |off: usize| -> Option<String> {
        let n = u16_at(off + 4)?;
        let t = (0..n).map(|i| off + 12 + 16 * i).find(|&r| b.get(r..r + 4) == Some(&b"name"[..]))?;
        let tab = u32_at(t + 8)?;
        let (count, strings) = (u16_at(tab + 2)?, tab + u16_at(tab + 4)?);
        let mut mac = None;
        for i in 0..count {
            let r = tab + 6 + 12 * i;
            let (plat, id, len, at) = (u16_at(r)?, u16_at(r + 6)?, u16_at(r + 8)?, u16_at(r + 10)?);
            let s = b.get(strings + at..strings + at + len)?;
            if id != 6 {
                continue;
            }
            if plat == 3 || plat == 0 {
                let w: Vec<u16> = s.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                return Some(String::from_utf16_lossy(&w));
            }
            if plat == 1 {
                mac = Some(s.iter().map(|&c| c as char).collect());
            }
        }
        mac
    };
    faces.into_iter().map(|o| name_of(o).unwrap_or_default()).collect()
}

/// the script of a CJK character: 0 Hangul, 1 kana, 2 Han, 3 Bopomofo, 4 CJK punctuation and full-width forms
fn script(c: char) -> Option<u8> {
    Some(match c as u32 {
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xFFA0..=0xFFDC => 0,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF65..=0xFF9F | 0x1B000..=0x1B16F => 1,
        0x2E80..=0x2FDF | 0x3190..=0x319F | 0x31C0..=0x31EF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3FFFF => 2,
        0x3100..=0x312F | 0x31A0..=0x31BF => 3,
        0x3000..=0x303F | 0x3200..=0x33FF | 0xFE30..=0xFE4F | 0xFF00..=0xFF64 | 0xFFE0..=0xFFEF => 4,
        _ => return None,
    })
}

/// a CJK font of the system, handed to egui for a character its built-in fonts do not have.
/// `strict`: only for the scripts of its language (so Han takes the font of the UI language first)
struct CjkFont {
    lang: Lang,
    strict: bool,
}

impl egui::FontProvider for CjkFont {
    fn font_for(&self, req: &egui::FallbackRequest<'_>) -> Option<egui::FontInsert> {
        let s = script(req.cluster.chars().next()?)?;
        let fits = match self.lang {
            Lang::Ko => matches!(s, 0 | 2 | 4),
            Lang::Ja => matches!(s, 1 | 2 | 4),
            _ => matches!(s, 2 | 3 | 4),
        };
        if self.strict && !fits {
            return None;
        }
        // the first time the window needs this font: how long drawing waited for it
        static WAITED: [AtomicBool; 3] = [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)];
        let t0 = std::time::Instant::now();
        let data = load(self.lang)?;
        if !WAITED[slot(self.lang)?].swap(true, Ordering::Relaxed) {
            eprintln!("lsv: {} font: drawing waited {} ms for it", self.lang.english(), t0.elapsed().as_millis());
        }
        Some(egui::FontInsert::new(&format!("lsv-cjk-{}", self.lang.code()), data, vec![]))
    }
}

/// (re)install the CJK font lookup: the UI language's font first, then Japanese, Chinese, Korean.
/// egui then rebuilds its fonts, so a new language also gets its own Han glyph forms.
pub fn install_fonts(ctx: &egui::Context) {
    let mut order = vec![Lang::Ja, Lang::Zh, Lang::Ko];
    if let Some(p) = order.iter().position(|l| *l == lang()) {
        let l = order.remove(p);
        order.insert(0, l);
    }
    let mut v: Vec<Arc<dyn egui::FontProvider>> = vec![];
    for strict in [true, false] {
        v.extend(order.iter().map(|&lang| Arc::new(CjkFont { lang, strict }) as Arc<dyn egui::FontProvider>));
    }
    ctx.set_font_providers(v);
}

/// a one-line note when the current language needs a font this computer does not have (in English,
/// since the text of that language would not show)
pub fn font_note() -> Option<String> {
    let l = lang();
    slot(l)?;
    if locate(l).is_some() {
        return None;
    }
    Some(format!("No {} font was found on this computer, so {} text shows as boxes. Another language: third menu (View) > Language.", l.english(), l.english()))
}

/// for the Language menu: the CJK languages whose names cannot be shown for want of a font
pub fn missing_fonts_note() -> Option<String> {
    let miss: Vec<&str> = [Lang::Ko, Lang::Ja, Lang::Zh].into_iter().filter(|&l| locate(l).is_none()).map(|l| l.english()).collect();
    (!miss.is_empty()).then(|| format!("No font found for {}: shown as boxes", miss.join(", ")))
}

// ---------- translations ----------
/// English, Korean, Japanese, Simplified Chinese, Spanish, German, French ("" = the English text)
const T: &[[&str; 7]] = &[
    // menu bar
    ["File", "파일", "ファイル", "文件", "Archivo", "Datei", "Fichier"],
    ["Edit", "편집", "編集", "编辑", "Editar", "Bearbeiten", "Édition"],
    ["View", "보기", "表示", "视图", "Ver", "Ansicht", "Affichage"],
    ["Help", "도움말", "ヘルプ", "帮助", "Ayuda", "Hilfe", "Aide"],
    ["Open…", "열기…", "開く…", "打开…", "Abrir…", "Öffnen…", "Ouvrir…"],
    ["Block", "블록", "ブロック", "模块", "Bloque", "Block", "Bloc"],
    ["Cell", "셀", "セル", "单元", "Celda", "Zelle", "Cellule"],
    ["Open Liberty (.lib)…", "Liberty 열기 (.lib)…", "Liberty を開く (.lib)…", "打开 Liberty (.lib)…", "Abrir Liberty (.lib)…", "Liberty öffnen (.lib)…", "Ouvrir un Liberty (.lib)…"],
    ["Open gate-level netlist (.v)…", "게이트 레벨 넷리스트 열기 (.v)…", "ゲートレベルネットリストを開く (.v)…", "打开门级网表 (.v)…", "Abrir netlist a nivel de puertas (.v)…", "Gatter-Netzliste öffnen (.v)…", "Ouvrir une netlist au niveau portes (.v)…"],
    ["Open LEF…", "LEF 열기…", "LEF を開く…", "打开 LEF…", "Abrir LEF…", "LEF öffnen…", "Ouvrir un LEF…"],
    ["Open DEF…", "DEF 열기…", "DEF を開く…", "打开 DEF…", "Abrir DEF…", "DEF öffnen…", "Ouvrir un DEF…"],
    ["Open SPEF (block parasitics)…", "SPEF 열기 (블록 기생 성분)…", "SPEF を開く（ブロックの寄生成分）…", "打开 SPEF（模块寄生参数）…", "Abrir SPEF (parásitos del bloque)…", "SPEF öffnen (Parasitics des Blocks)…", "Ouvrir un SPEF (parasites du bloc)…"],
    ["Open CDL / SPICE / LPE netlist…", "CDL / SPICE / LPE 넷리스트 열기…", "CDL / SPICE / LPE ネットリストを開く…", "打开 CDL / SPICE / LPE 网表…", "Abrir netlist CDL / SPICE / LPE…", "CDL-/SPICE-/LPE-Netzliste öffnen…", "Ouvrir une netlist CDL / SPICE / LPE…"],
    ["Open GDSII…", "GDSII 열기…", "GDSII を開く…", "打开 GDSII…", "Abrir GDSII…", "GDSII öffnen…", "Ouvrir un GDSII…"],
    ["Open cell LEF…", "셀 LEF 열기…", "セル LEF を開く…", "打开单元 LEF…", "Abrir LEF de celdas…", "Zell-LEF öffnen…", "Ouvrir un LEF de cellules…"],
    ["Open layer map…", "레이어 맵 열기…", "レイヤマップを開く…", "打开层映射…", "Abrir mapa de capas…", "Lagenzuordnung öffnen…", "Ouvrir une table des couches…"],
    ["Layer map (.layermap, .lyp)", "레이어 맵 (.layermap, .lyp)", "レイヤマップ (.layermap, .lyp)", "层映射 (.layermap, .lyp)", "Mapa de capas (.layermap, .lyp)", "Lagenzuordnung (.layermap, .lyp)", "Table des couches (.layermap, .lyp)"],
    ["Reload all files", "모든 파일 다시 읽기", "すべてのファイルを再読み込み", "重新加载所有文件", "Volver a cargar todos los archivos", "Alle Dateien neu laden", "Recharger tous les fichiers"],
    ["Exit", "끝내기", "終了", "退出", "Salir", "Beenden", "Quitter"],
    ["Find…", "찾기…", "検索…", "查找…", "Buscar…", "Suchen…", "Rechercher…"],
    ["Copy selected name", "선택한 이름 복사", "選択した名前をコピー", "复制所选名称", "Copiar el nombre seleccionado", "Ausgewählten Namen kopieren", "Copier le nom sélectionné"],
    ["Clear selection", "선택 해제", "選択を解除", "清除选择", "Quitar la selección", "Auswahl aufheben", "Effacer la sélection"],
    ["Netlist", "넷리스트", "ネットリスト", "网表", "Netlist", "Netzliste", "Netlist"],
    ["Layout", "레이아웃", "レイアウト", "版图", "Layout", "Layout", "Layout"],
    ["Schematic", "회로도", "回路図", "原理图", "Esquema", "Schaltplan", "Schéma"],
    ["Parasitics (LPE)", "기생 성분 (LPE)", "寄生成分 (LPE)", "寄生参数 (LPE)", "Parásitos (LPE)", "Parasitäre Elemente (LPE)", "Parasites (LPE)"],
    ["Schematic and layout side by side", "회로도와 레이아웃 나란히", "回路図とレイアウトを並べて表示", "原理图与版图并排", "Esquema y layout lado a lado", "Schaltplan und Layout nebeneinander", "Schéma et layout côte à côte"],
    ["Own window", "별도 창", "別ウィンドウ", "独立窗口", "Ventana propia", "Eigenes Fenster", "Fenêtre séparée"],
    ["Dock all windows", "모든 창 다시 붙이기", "すべてのウィンドウを戻す", "收回所有窗口", "Acoplar todas las ventanas", "Alle Fenster andocken", "Réancrer toutes les fenêtres"],
    ["Reset layout", "화면 배치 초기화", "配置をリセット", "重置界面布局", "Restablecer la disposición", "Anordnung zurücksetzen", "Réinitialiser la disposition"],
    ["Fit to window", "화면에 맞추기", "ウィンドウに合わせる", "适应窗口", "Ajustar a la ventana", "An Fenster anpassen", "Ajuster à la fenêtre"],
    ["Details panel", "상세 정보 패널", "詳細パネル", "详细信息面板", "Panel de detalles", "Detailbereich", "Panneau des détails"],
    ["Theme", "테마", "テーマ", "主题", "Tema", "Farbschema", "Thème"],
    ["Follow system", "시스템 설정 따르기", "システムに合わせる", "跟随系统", "Según el sistema", "Wie das System", "Suivre le système"],
    ["Light", "밝게", "ライト", "浅色", "Claro", "Hell", "Clair"],
    ["Dark", "어둡게", "ダーク", "深色", "Oscuro", "Dunkel", "Sombre"],
    ["Language", "언어", "言語", "语言", "Idioma", "Sprache", "Langue"],
    ["Keyboard shortcuts", "단축키", "キーボードショートカット", "键盘快捷键", "Atajos de teclado", "Tastenkürzel", "Raccourcis clavier"],
    ["About", "정보", "このアプリについて", "关于", "Acerca de", "Über", "À propos"],
    ["Could not save the language choice: {}", "언어 선택을 저장하지 못했어요: {}", "言語の選択を保存できませんでした: {}", "无法保存语言选择：{}", "No se pudo guardar el idioma elegido: {}", "Die Sprachwahl konnte nicht gespeichert werden: {}", "Impossible d'enregistrer le choix de langue : {}"],
    // toolbar and view panes
    ["BLOCK", "블록", "ブロック", "模块", "BLOQUE", "BLOCK", "BLOC"],
    ["CELL", "셀", "セル", "单元", "CELDA", "ZELLE", "CELLULE"],
    ["Block layout", "블록 레이아웃", "ブロックレイアウト", "模块版图", "Layout del bloque", "Block-Layout", "Layout du bloc"],
    ["Cell layout", "셀 레이아웃", "セルレイアウト", "单元版图", "Layout de la celda", "Zell-Layout", "Layout de la cellule"],
    ["Cell LEF", "셀 LEF", "セル LEF", "单元 LEF", "LEF de la celda", "Zell-LEF", "LEF de la cellule"],
    ["Parasitics", "기생 성분", "寄生成分", "寄生参数", "Parásitos", "Parasitäre Elemente", "Parasites"],
    ["Details", "상세 정보", "詳細", "详细信息", "Detalles", "Details", "Détails"],
    ["Fit", "맞추기", "全体表示", "适应", "Ajustar", "Einpassen", "Ajuster"],
    ["Fit to window (F)", "화면에 맞추기 (F)", "ウィンドウに合わせる (F)", "适应窗口 (F)", "Ajustar a la ventana (F)", "An Fenster anpassen (F)", "Ajuster à la fenêtre (F)"],
    ["Search cells and nets  (Ctrl+F)", "셀과 넷 검색  (Ctrl+F)", "セルとネットを検索  (Ctrl+F)", "搜索单元和线网  (Ctrl+F)", "Buscar celdas y redes  (Ctrl+F)", "Zellen und Netze suchen  (Ctrl+F)", "Rechercher cellules et nets  (Ctrl+F)"],
    ["Open in its own window", "별도 창으로 열기", "別ウィンドウで開く", "在独立窗口中打开", "Abrir en su propia ventana", "In eigenem Fenster öffnen", "Ouvrir dans sa propre fenêtre"],
    // keyboard shortcuts window
    ["Open files", "파일 열기", "ファイルを開く", "打开文件", "Abrir archivos", "Dateien öffnen", "Ouvrir des fichiers"],
    ["Find cells, nets, transistors", "셀, 넷, 트랜지스터 찾기", "セル・ネット・トランジスタを検索", "查找单元、线网、晶体管", "Buscar celdas, redes, transistores", "Zellen, Netze, Transistoren suchen", "Rechercher cellules, nets, transistors"],
    ["Copy the selected name", "선택한 이름 복사", "選択した名前をコピー", "复制所选名称", "Copiar el nombre seleccionado", "Ausgewählten Namen kopieren", "Copier le nom sélectionné"],
    ["Clear the selection", "선택 해제", "選択を解除", "清除选择", "Quitar la selección", "Auswahl aufheben", "Effacer la sélection"],
    ["Block netlist / layout", "블록 넷리스트 / 레이아웃", "ブロックのネットリスト / レイアウト", "模块网表 / 版图", "Netlist / layout del bloque", "Block: Netzliste / Layout", "Netlist / layout du bloc"],
    ["Cell schematic / layout / LEF", "셀 회로도 / 레이아웃 / LEF", "セルの回路図 / レイアウト / LEF", "单元原理图 / 版图 / LEF", "Esquema / layout / LEF de la celda", "Zelle: Schaltplan / Layout / LEF", "Schéma / layout / LEF de la cellule"],
    ["Drag a view tab", "보기 탭 끌기", "ビューのタブをドラッグ", "拖动视图标签", "Arrastrar una pestaña", "Ansichts-Tab ziehen", "Faire glisser un onglet"],
    ["Drop it on an edge of a view to split, on a tab bar to stack", "보기 가장자리에 놓으면 나눠서, 탭 줄에 놓으면 탭으로 겹쳐서 보여요", "ビューの端に落とすと分割、タブバーに落とすとタブで重ねます", "拖到视图边缘可拆分，拖到标签栏可叠放", "Suéltala en el borde de una vista para dividir, en una barra de pestañas para apilar", "Am Rand einer Ansicht ablegen zum Teilen, auf einer Tab-Leiste zum Stapeln", "Déposez-le au bord d'une vue pour la diviser, sur une barre d'onglets pour l'empiler"],
    ["Right click a view tab", "보기 탭 우클릭", "ビューのタブを右クリック", "右键点击视图标签", "Clic derecho en una pestaña", "Rechtsklick auf einen Ansichts-Tab", "Clic droit sur un onglet"],
    ["Open the view in its own window", "그 보기를 별도 창으로 열기", "そのビューを別ウィンドウで開く", "在独立窗口中打开该视图", "Abrir la vista en su propia ventana", "Ansicht in eigenem Fenster öffnen", "Ouvrir la vue dans sa propre fenêtre"],
    ["Show or hide the side panel", "옆 패널 보이기/숨기기", "サイドパネルの表示/非表示", "显示或隐藏侧边栏", "Mostrar u ocultar el panel lateral", "Seitenleiste ein- oder ausblenden", "Afficher ou masquer le panneau latéral"],
    ["Wheel", "휠", "ホイール", "滚轮", "Rueda", "Mausrad", "Molette"],
    ["Zoom at the pointer", "포인터 위치에서 확대/축소", "ポインタの位置で拡大・縮小", "以指针为中心缩放", "Zoom en el puntero", "Am Mauszeiger zoomen", "Zoomer sous le pointeur"],
    ["Wheel-button drag", "휠 버튼 끌기", "ホイールボタンでドラッグ", "按住滚轮拖动", "Arrastrar con la rueda", "Mit gedrücktem Mausrad ziehen", "Glisser avec la molette enfoncée"],
    ["Pan (hold the wheel down and move)", "이동 (휠을 누른 채 움직이기)", "移動（ホイールを押したまま動かす）", "平移（按住滚轮并移动）", "Desplazar (mantén pulsada la rueda y mueve)", "Verschieben (Mausrad gedrückt halten und bewegen)", "Déplacer (maintenez la molette enfoncée et bougez)"],
    ["Click", "클릭", "クリック", "点击", "Clic", "Klick", "Clic"],
    ["Select", "선택", "選択", "选择", "Seleccionar", "Auswählen", "Sélectionner"],
    ["Right click", "우클릭", "右クリック", "右键点击", "Clic derecho", "Rechtsklick", "Clic droit"],
    ["Jump to the other views", "다른 보기로 이동", "他のビューへ移動", "跳转到其他视图", "Saltar a las otras vistas", "Zu den anderen Ansichten springen", "Aller aux autres vues"],
    // About
    ["Block: gate-level netlist from Liberty + Verilog, and LEF/DEF layout on the GPU.", "블록: Liberty + Verilog로 읽은 게이트 레벨 넷리스트, 그리고 GPU로 그리는 LEF/DEF 레이아웃.", "ブロック: Liberty + Verilog から読むゲートレベルネットリストと、GPU で描く LEF/DEF レイアウト。", "模块：由 Liberty + Verilog 读入的门级网表，以及在 GPU 上绘制的 LEF/DEF 版图。", "Bloque: netlist a nivel de puertas desde Liberty + Verilog, y layout LEF/DEF en la GPU.", "Block: Gatter-Netzliste aus Liberty + Verilog und LEF/DEF-Layout auf der GPU.", "Bloc : netlist au niveau portes depuis Liberty + Verilog, et layout LEF/DEF sur le GPU."],
    ["Cell: CDL transistor schematic and GDS layout with an LVS-style match.", "셀: CDL 트랜지스터 회로도와 GDS 레이아웃을 LVS 방식으로 맞춰 봐요.", "セル: CDL のトランジスタ回路図と GDS レイアウトを LVS 方式で対応付けます。", "单元：CDL 晶体管原理图与 GDS 版图，按 LVS 方式对应。", "Celda: esquema de transistores CDL y layout GDS emparejados al estilo LVS.", "Zelle: CDL-Transistorschaltplan und GDS-Layout mit Abgleich wie bei LVS.", "Cellule : schéma de transistors CDL et layout GDS appariés à la manière d'un LVS."],
    ["Everything is cross-probed. Drop files on the window or use File > Open.", "모든 보기가 서로 연동돼요. 파일을 창에 끌어다 놓거나 파일 > 열기를 쓰세요.", "すべてのビューが連動します。ファイルをウィンドウにドロップするか、ファイル > 開く を使ってください。", "所有视图相互联动。把文件拖到窗口上，或使用 文件 > 打开。", "Todo está enlazado entre vistas. Suelta archivos en la ventana o usa Archivo > Abrir.", "Alle Ansichten sind verknüpft. Dateien auf das Fenster ziehen oder Datei > Öffnen verwenden.", "Toutes les vues sont liées. Déposez des fichiers sur la fenêtre ou utilisez Fichier > Ouvrir."],
    // status and progress
    ["Open files from the File menu, or drop them on the window.", "파일 메뉴에서 파일을 열거나 창에 끌어다 놓으세요.", "ファイルメニューから開くか、ウィンドウにドロップしてください。", "从“文件”菜单打开文件，或把文件拖到窗口上。", "Abre archivos desde el menú Archivo o suéltalos en la ventana.", "Dateien über das Menü Datei öffnen oder auf das Fenster ziehen.", "Ouvrez des fichiers depuis le menu Fichier ou déposez-les sur la fenêtre."],
    ["Reading {}", "읽는 중: {}", "読み込み中: {}", "正在读取：{}", "Leyendo {}", "Lese {}", "Lecture de {}"],
    ["no DEF file", "DEF 파일 없음", "DEF ファイルがありません", "没有 DEF 文件", "no hay archivo DEF", "keine DEF-Datei", "aucun fichier DEF"],
    ["Parsing {} MB of DEF on {} threads", "DEF {} MB 해석 중 (스레드 {}개)", "DEF {} MB を解析中（{} スレッド）", "正在解析 {} MB 的 DEF（{} 个线程）", "Analizando {} MB de DEF en {} hilos", "{} MB DEF werden auf {} Threads gelesen", "Analyse de {} Mo de DEF sur {} threads"],
    ["Building the scene ({} shapes)", "화면 데이터 만드는 중 (도형 {}개)", "シーンを作成中（図形 {} 個）", "正在构建场景（{} 个图形）", "Construyendo la escena ({} formas)", "Szene wird aufgebaut ({} Formen)", "Construction de la scène ({} formes)"],
    ["{}: {} cells, {} nets, {} shapes | read {:.1} s, parse {:.1} s, scene {:.1} s | warnings {}", "{}: 셀 {}개, 넷 {}개, 도형 {}개 | 읽기 {:.1} s, 해석 {:.1} s, 화면 {:.1} s | 경고 {}", "{}: セル {} 個、ネット {} 個、図形 {} 個 | 読み込み {:.1} s、解析 {:.1} s、シーン {:.1} s | 警告 {}", "{}：{} 个单元，{} 个线网，{} 个图形 | 读取 {:.1} s，解析 {:.1} s，场景 {:.1} s | 警告 {}", "{}: {} celdas, {} redes, {} formas | lectura {:.1} s, análisis {:.1} s, escena {:.1} s | avisos {}", "{}: {} Zellen, {} Netze, {} Formen | Lesen {:.1} s, Parsen {:.1} s, Szene {:.1} s | Warnungen {}", "{} : {} cellules, {} nets, {} formes | lecture {:.1} s, analyse {:.1} s, scène {:.1} s | avertissements {}"],
    ["no netlist", "넷리스트 없음", "ネットリストがありません", "没有网表", "no hay netlist", "keine Netzliste", "aucune netlist"],
    ["Elaborating {}", "{} 엘라보레이션 중", "{} をエラボレーション中", "正在展开 {}", "Elaborando {}", "{} wird elaboriert", "Élaboration de {}"],
    ["{}: {} cells, {} nets, {} ports | netlist {:.1} s | warnings {}", "{}: 셀 {}개, 넷 {}개, 포트 {}개 | 넷리스트 {:.1} s | 경고 {}", "{}: セル {} 個、ネット {} 個、ポート {} 個 | ネットリスト {:.1} s | 警告 {}", "{}：{} 个单元，{} 个线网，{} 个端口 | 网表 {:.1} s | 警告 {}", "{}: {} celdas, {} redes, {} puertos | netlist {:.1} s | avisos {}", "{}: {} Zellen, {} Netze, {} Ports | Netzliste {:.1} s | Warnungen {}", "{} : {} cellules, {} nets, {} ports | netlist {:.1} s | avertissements {}"],
    ["Loading the layout…", "레이아웃 읽는 중…", "レイアウトを読み込み中…", "正在加载版图…", "Cargando el layout…", "Layout wird geladen…", "Chargement du layout…"],
    ["Loading the netlist…", "넷리스트 읽는 중…", "ネットリストを読み込み中…", "正在加载网表…", "Cargando la netlist…", "Netzliste wird geladen…", "Chargement de la netlist…"],
    ["Those files are not LEF, DEF, Liberty, Verilog, CDL, GDS or a layer map.", "이 파일들은 LEF, DEF, Liberty, Verilog, CDL, GDS, 레이어 맵 중 어느 것도 아니에요.", "これらのファイルは LEF、DEF、Liberty、Verilog、CDL、GDS、レイヤマップのどれでもありません。", "这些文件不是 LEF、DEF、Liberty、Verilog、CDL、GDS 或层映射。", "Esos archivos no son LEF, DEF, Liberty, Verilog, CDL, GDS ni un mapa de capas.", "Diese Dateien sind weder LEF, DEF, Liberty, Verilog, CDL, GDS noch eine Lagenzuordnung.", "Ces fichiers ne sont ni LEF, ni DEF, ni Liberty, ni Verilog, ni CDL, ni GDS, ni une table des couches."],
    ["All supported files", "지원하는 모든 파일", "対応するすべてのファイル", "所有支持的文件", "Todos los archivos admitidos", "Alle unterstützten Dateien", "Tous les fichiers pris en charge"],
    ["All files", "모든 파일", "すべてのファイル", "所有文件", "Todos los archivos", "Alle Dateien", "Tous les fichiers"],
    ["Open design files", "설계 파일 열기", "設計ファイルを開く", "打开设计文件", "Abrir archivos de diseño", "Entwurfsdateien öffnen", "Ouvrir des fichiers de conception"],
    ["Open cell LEF", "셀 LEF 열기", "セル LEF を開く", "打开单元 LEF", "Abrir LEF de celdas", "Zell-LEF öffnen", "Ouvrir un LEF de cellules"],
    ["Nothing to reload: no files are open.", "열린 파일이 없어서 다시 읽을 게 없어요.", "開いているファイルがないため、再読み込みするものはありません。", "没有打开的文件，无需重新加载。", "Nada que volver a cargar: no hay archivos abiertos.", "Nichts neu zu laden: keine Dateien geöffnet.", "Rien à recharger : aucun fichier n'est ouvert."],
    ["Copied \"{}\"", "“{}” 복사했어요", "「{}」をコピーしました", "已复制“{}”", "Copiado «{}»", "„{}“ kopiert", "« {} » copié"],
    ["Netlist: {} cells of {} drawn ({}) | layout {} ms", "넷리스트: 셀 {1}개 중 {0}개 그림 ({2}) | 배치 {3} ms", "ネットリスト: セル {1} 個中 {0} 個を描画（{2}） | 配置 {3} ms", "网表：已绘制 {1} 个单元中的 {0} 个（{2}） | 布局 {3} ms", "Netlist: {} de {} celdas dibujadas ({}) | colocación {} ms", "Netzliste: {} von {} Zellen gezeichnet ({}) | Anordnung {} ms", "Netlist : {} cellules sur {} dessinées ({}) | placement {} ms"],
    ["whole design", "전체 설계", "設計全体", "整个设计", "diseño completo", "ganzer Entwurf", "conception entière"],
    ["around the selection", "선택 주변", "選択の周辺", "所选对象周围", "alrededor de la selección", "um die Auswahl", "autour de la sélection"],
    ["Load a Liberty file and a netlist to see the schematic.", "회로도를 보려면 Liberty 파일과 넷리스트를 읽어 주세요.", "回路図を見るには Liberty ファイルとネットリストを読み込んでください。", "请加载 Liberty 文件和网表以查看原理图。", "Carga un archivo Liberty y una netlist para ver el esquema.", "Liberty-Datei und Netzliste laden, um den Schaltplan zu sehen.", "Chargez un fichier Liberty et une netlist pour voir le schéma."],
    ["{} is not in the netlist (physical-only cells such as fillers and taps have no schematic).", "{}: 넷리스트에 없어요 (필러, 탭처럼 물리적으로만 있는 셀은 회로도가 없어요).", "{} はネットリストにありません（フィラーやタップなど物理的にだけ存在するセルには回路図がありません）。", "网表中没有 {}（填充单元、tap 单元等仅有物理形式的单元没有原理图）。", "{} no está en la netlist (las celdas solo físicas, como rellenos y taps, no tienen esquema).", "{} ist nicht in der Netzliste (rein physische Zellen wie Filler und Taps haben keinen Schaltplan).", "{} n'est pas dans la netlist (les cellules purement physiques, comme les fillers et les taps, n'ont pas de schéma)."],
    ["Load LEF and DEF files to see the layout.", "레이아웃을 보려면 LEF와 DEF 파일을 읽어 주세요.", "レイアウトを見るには LEF と DEF ファイルを読み込んでください。", "请加载 LEF 和 DEF 文件以查看版图。", "Carga archivos LEF y DEF para ver el layout.", "LEF- und DEF-Dateien laden, um das Layout zu sehen.", "Chargez des fichiers LEF et DEF pour voir le layout."],
    ["{} is not in the layout.", "{}: 레이아웃에 없어요.", "{} はレイアウトにありません。", "版图中没有 {}。", "{} no está en el layout.", "{} ist nicht im Layout.", "{} n'est pas dans le layout."],
    ["Error: {}", "오류: {}", "エラー: {}", "错误：{}", "Error: {}", "Fehler: {}", "Erreur : {}"],
    ["{} | GPU upload {:.1} s", "{} | GPU 업로드 {:.1} s", "{} | GPU 転送 {:.1} s", "{} | GPU 上传 {:.1} s", "{} | subida a la GPU {:.1} s", "{} | GPU-Upload {:.1} s", "{} | envoi au GPU {:.1} s"],
    ["pin {}", "핀 {}", "ピン {}", "引脚 {}", "pin {}", "Pin {}", "broche {}"],
    // details pane
    ["Search", "검색", "検索", "搜索", "Búsqueda", "Suche", "Recherche"],
    ["cell ", "셀 ", "セル ", "单元 ", "celda ", "Zelle ", "cellule "],
    ["xtor ", "트랜지스터 ", "トランジスタ ", "晶体管 ", "transistor ", "Transistor ", "transistor "],
    ["net  ", "넷 ", "ネット ", "线网 ", "red ", "Netz ", "net "],
    ["cell  {}  ·  {}", "셀  {}  ·  {}", "セル  {}  ·  {}", "单元  {}  ·  {}", "celda  {}  ·  {}", "Zelle  {}  ·  {}", "cellule  {}  ·  {}"],
    ["net   {}", "넷  {}", "ネット  {}", "线网  {}", "red  {}", "Netz  {}", "net  {}"],
    ["Open a Liberty file and a gate-level netlist.", "Liberty 파일과 게이트 레벨 넷리스트를 여세요.", "Liberty ファイルとゲートレベルネットリストを開いてください。", "请打开 Liberty 文件和门级网表。", "Abre un archivo Liberty y una netlist a nivel de puertas.", "Liberty-Datei und Gatter-Netzliste öffnen.", "Ouvrez un fichier Liberty et une netlist au niveau portes."],
    ["{} cells: too many for one sheet, so the view shows the cells around the selection.", "셀 {}개: 한 장에 그리기엔 너무 많아서 선택 주변 셀만 보여요.", "セル {} 個: 1 枚に描くには多すぎるため、選択の周辺のセルだけを表示します。", "{} 个单元：太多，无法画在一张图上，因此只显示所选对象周围的单元。", "{} celdas: demasiadas para una hoja, así que la vista muestra las celdas alrededor de la selección.", "{} Zellen: zu viele für ein Blatt, daher zeigt die Ansicht die Zellen um die Auswahl.", "{} cellules : trop pour une seule feuille, la vue montre donc les cellules autour de la sélection."],
    ["Whole design", "전체 설계", "設計全体", "整个设计", "Diseño completo", "Ganzer Entwurf", "Conception entière"],
    ["Around the selection", "선택 주변", "選択の周辺", "所选对象周围", "Alrededor de la selección", "Um die Auswahl", "Autour de la sélection"],
    ["fan-in", "", "", "扇入", "", "Fan-in", ""],
    ["fan-out", "", "", "扇出", "", "Fan-out", ""],
    ["Redraw", "다시 그리기", "再描画", "重绘", "Redibujar", "Neu zeichnen", "Redessiner"],
    ["Selection", "선택", "選択", "选择", "Selección", "Auswahl", "Sélection"],
    ["{} drivers · {} sinks", "driver {}개 · sink {}개", "ドライバ {} 個 · シンク {} 個", "{} 个驱动 · {} 个负载", "{} drivers · {} receptores", "{} Treiber · {} Senken", "{} pilotes · {} charges"],
    ["port {} ({})", "포트 {} ({})", "ポート {} ({})", "端口 {} ({})", "puerto {} ({})", "Port {} ({})", "port {} ({})"],
    ["constant {}", "상수 {}", "定数 {}", "常量 {}", "constante {}", "Konstante {}", "constante {}"],
    ["Show in layout", "레이아웃에서 보기", "レイアウトで表示", "在版图中查看", "Ver en el layout", "Im Layout zeigen", "Voir dans le layout"],
    ["Show in netlist", "넷리스트에서 보기", "ネットリストで表示", "在网表中查看", "Ver en la netlist", "In der Netzliste zeigen", "Voir dans la netlist"],
    ["Show in schematic", "회로도에서 보기", "回路図で表示", "在原理图中查看", "Ver en el esquema", "Im Schaltplan zeigen", "Voir dans le schéma"],
    ["Show {} transistors", "{} 트랜지스터 보기", "{} のトランジスタを表示", "查看 {} 的晶体管", "Ver los transistores de {}", "Transistoren von {} zeigen", "Voir les transistors de {}"],
    ["Click a gate or a wire.", "게이트나 선을 누르세요.", "ゲートか配線をクリックしてください。", "请点击门或连线。", "Haz clic en una puerta o un cable.", "Auf ein Gatter oder eine Leitung klicken.", "Cliquez sur une porte ou un fil."],
    ["Show", "표시", "表示", "显示", "Mostrar", "Anzeigen", "Afficher"],
    ["Cells", "셀", "セル", "单元", "Celdas", "Zellen", "Cellules"],
    ["Signal routing", "신호 배선", "信号配線", "信号走线", "Rutado de señal", "Signalverdrahtung", "Routage signal"],
    ["Power routing", "전원 배선", "電源配線", "电源走线", "Rutado de alimentación", "Versorgungsverdrahtung", "Routage alimentation"],
    ["Top-level pins", "최상위 핀", "トップレベルピン", "顶层引脚", "Pines de nivel superior", "Top-Level-Pins", "Broches de niveau supérieur"],
    ["Layers", "레이어", "レイヤ", "层", "Capas", "Lagen", "Couches"],
    ["location {:.3}, {:.3} µm · {} · {}", "위치 {:.3}, {:.3} µm · {} · {}", "位置 {:.3}, {:.3} µm · {} · {}", "位置 {:.3}, {:.3} µm · {} · {}", "posición {:.3}, {:.3} µm · {} · {}", "Position {:.3}, {:.3} µm · {} · {}", "position {:.3}, {:.3} µm · {} · {}"],
    ["use {} · {} connections · {} shapes", "용도 {} · 연결 {}개 · 도형 {}개", "用途 {} · 接続 {} 個 · 図形 {} 個", "用途 {} · {} 个连接 · {} 个图形", "uso {} · {} conexiones · {} formas", "Verwendung {} · {} Verbindungen · {} Formen", "usage {} · {} connexions · {} formes"],
    ["top-level pin · {} · net {}", "최상위 핀 · {} · 넷 {}", "トップレベルピン · {} · ネット {}", "顶层引脚 · {} · 线网 {}", "pin de nivel superior · {} · red {}", "Top-Level-Pin · {} · Netz {}", "broche de niveau supérieur · {} · net {}"],
    ["Click a cell or a wire.", "셀이나 선을 누르세요.", "セルか配線をクリックしてください。", "请点击单元或连线。", "Haz clic en una celda o un cable.", "Auf eine Zelle oder eine Leitung klicken.", "Cliquez sur une cellule ou un fil."],
    // canvases
    ["Drop LEF and DEF files here", "LEF와 DEF 파일을 여기에 놓으세요", "ここに LEF と DEF ファイルをドロップ", "把 LEF 和 DEF 文件拖到这里", "Suelta aquí archivos LEF y DEF", "LEF- und DEF-Dateien hier ablegen", "Déposez ici des fichiers LEF et DEF"],
    ["Nothing here", "여기엔 아무것도 없어요", "ここには何もありません", "这里什么也没有", "Aquí no hay nada", "Hier ist nichts", "Rien ici"],
    ["Nothing to draw", "그릴 게 없어요", "描画するものがありません", "没有可绘制的内容", "Nada que dibujar", "Nichts zu zeichnen", "Rien à dessiner"],
    ["Drop a Liberty file and a gate-level netlist here", "Liberty 파일과 게이트 레벨 넷리스트를 여기에 놓으세요", "ここに Liberty ファイルとゲートレベルネットリストをドロップ", "把 Liberty 文件和门级网表拖到这里", "Suelta aquí un archivo Liberty y una netlist a nivel de puertas", "Liberty-Datei und Gatter-Netzliste hier ablegen", "Déposez ici un fichier Liberty et une netlist au niveau portes"],
    ["overview of {} cells: zoom in for gate symbols and names", "셀 {}개 개요: 확대하면 게이트 기호와 이름이 보여요", "セル {} 個の概観: 拡大するとゲート記号と名前が表示されます", "{} 个单元的概览：放大可看到门符号和名称", "vista general de {} celdas: acerca para ver los símbolos y nombres", "Übersicht über {} Zellen: hineinzoomen für Gattersymbole und Namen", "vue d'ensemble de {} cellules : zoomez pour voir les symboles et les noms"],
    // cell views
    ["ASAP7 (built in)", "ASAP7 (내장)", "ASAP7（内蔵）", "ASAP7（内置）", "ASAP7 (integrado)", "ASAP7 (eingebaut)", "ASAP7 (intégré)"],
    ["Reading the CDL / GDS…", "CDL / GDS 읽는 중…", "CDL / GDS を読み込み中…", "正在读取 CDL / GDS…", "Leyendo el CDL / GDS…", "CDL / GDS wird gelesen…", "Lecture du CDL / GDS…"],
    ["CDL {}: {} subcircuits{}", "CDL {}: 서브서킷 {}개{}", "CDL {}: サブサーキット {} 個{}", "CDL {}：{} 个子电路{}", "CDL {}: {} subcircuitos{}", "CDL {}: {} Subcircuits{}", "CDL {} : {} sous-circuits{}"],
    [", {} warnings", ", 경고 {}개", "、警告 {} 件", "，{} 条警告", ", {} avisos", ", {} Warnungen", ", {} avertissements"],
    ["GDS {}: {} cells", "GDS {}: 셀 {}개", "GDS {}: セル {} 個", "GDS {}：{} 个单元", "GDS {}: {} celdas", "GDS {}: {} Zellen", "GDS {} : {} cellules"],
    ["LEF {}: {} macros", "LEF {}: 매크로 {}개", "LEF {}: マクロ {} 個", "LEF {}：{} 个宏", "LEF {}: {} macros", "LEF {}: {} Makros", "LEF {} : {} macros"],
    ["layer map {}", "레이어 맵 {}", "レイヤマップ {}", "层映射 {}", "mapa de capas {}", "Lagenzuordnung {}", "table des couches {}"],
    ["built in", "내장", "内蔵", "内置", "integrado", "eingebaut", "intégré"],
    ["LSV_PDK_PATH file", "LSV_PDK_PATH 파일", "LSV_PDK_PATH のファイル", "LSV_PDK_PATH 文件", "archivo de LSV_PDK_PATH", "Datei aus LSV_PDK_PATH", "fichier de LSV_PDK_PATH"],
    ["{} ({}, detected)", "{} ({}, 자동 감지)", "{}（{}、自動検出）", "{}（{}，自动检测）", "{} ({}, detectado)", "{} ({}, erkannt)", "{} ({}, détecté)"],
    ["{} is not in the loaded CDL or GDS", "{}: 읽은 CDL이나 GDS에 없어요", "{} は読み込んだ CDL にも GDS にもありません", "已加载的 CDL 或 GDS 中没有 {}", "{} no está en el CDL ni en el GDS cargados", "{} ist weder im geladenen CDL noch im GDS", "{} n'est ni dans le CDL ni dans le GDS chargés"],
    ["{}: in the GDS but not in the CDL, so there is no schematic to match", "{}: GDS에는 있지만 CDL에 없어서 맞춰 볼 회로도가 없어요", "{}: GDS にはありますが CDL にないため、照合する回路図がありません", "{}：在 GDS 中但不在 CDL 中，因此没有可对照的原理图", "{}: está en el GDS pero no en el CDL, así que no hay esquema con el que comparar", "{}: im GDS, aber nicht im CDL, daher gibt es keinen Schaltplan zum Abgleich", "{} : dans le GDS mais pas dans le CDL, il n'y a donc pas de schéma à comparer"],
    ["{}: in the CDL but not in the GDS", "{}: CDL에는 있지만 GDS에는 없어요", "{}: CDL にはありますが GDS にはありません", "{}：在 CDL 中但不在 GDS 中", "{}: está en el CDL pero no en el GDS", "{}: im CDL, aber nicht im GDS", "{} : dans le CDL mais pas dans le GDS"],
    ["{}: {} transistors | {} | {:.0} ms", "{}: 트랜지스터 {}개 | {} | {:.0} ms", "{}: トランジスタ {} 個 | {} | {:.0} ms", "{}：{} 个晶体管 | {} | {:.0} ms", "{}: {} transistores | {} | {:.0} ms", "{}: {} Transistoren | {} | {:.0} ms", "{} : {} transistors | {} | {:.0} ms"],
    ["no CDL subcircuit for this cell", "이 셀의 CDL 서브서킷 없음", "このセルの CDL サブサーキットなし", "此单元没有 CDL 子电路", "no hay subcircuito CDL para esta celda", "kein CDL-Subcircuit für diese Zelle", "aucun sous-circuit CDL pour cette cellule"],
    ["no GDS cell for this subcircuit", "이 서브서킷의 GDS 셀 없음", "このサブサーキットの GDS セルなし", "此子电路没有 GDS 单元", "no hay celda GDS para este subcircuito", "keine GDS-Zelle für diesen Subcircuit", "aucune cellule GDS pour ce sous-circuit"],
    ["extraction rules: {} errors", "추출 규칙: 오류 {}개", "抽出ルール: エラー {} 件", "提取规则：{} 个错误", "reglas de extracción: {} errores", "Extraktionsregeln: {} Fehler", "règles d'extraction : {} erreurs"],
    ["layout matches the CDL: {} transistors, {} nets", "레이아웃이 CDL과 일치: 트랜지스터 {}개, 넷 {}개", "レイアウトは CDL と一致: トランジスタ {} 個、ネット {} 個", "版图与 CDL 一致：{} 个晶体管，{} 个线网", "el layout coincide con el CDL: {} transistores, {} redes", "Layout stimmt mit dem CDL überein: {} Transistoren, {} Netze", "le layout correspond au CDL : {} transistors, {} nets"],
    ["layout does NOT match the CDL: {} of {} device groups matched", "레이아웃이 CDL과 일치하지 않음: 장치 묶음 {1}개 중 {0}개만 대응", "レイアウトは CDL と一致しません: デバイスグループ {1} 個中 {0} 個のみ対応", "版图与 CDL 不一致：{1} 个器件组中只有 {0} 个对应", "el layout NO coincide con el CDL: {} de {} grupos de dispositivos emparejados", "Layout stimmt NICHT mit dem CDL überein: {} von {} Bauteilgruppen zugeordnet", "le layout ne correspond PAS au CDL : {} groupes de composants appariés sur {}"],
    ["Reading…", "읽는 중…", "読み込み中…", "正在读取…", "Leyendo…", "Wird gelesen…", "Lecture…"],
    ["Open a CDL netlist and/or a GDS file (CDL… / GDS… above).", "CDL 넷리스트나 GDS 파일, 또는 둘 다 여세요 (위의 CDL… / GDS…).", "CDL ネットリストと GDS ファイルのどちらか、または両方を開いてください（上の CDL… / GDS…）。", "请打开 CDL 网表和/或 GDS 文件（上方的 CDL… / GDS…）。", "Abre una netlist CDL y/o un archivo GDS (CDL… / GDS… arriba).", "CDL-Netzliste und/oder GDS-Datei öffnen (CDL… / GDS… oben).", "Ouvrez une netlist CDL et/ou un fichier GDS (CDL… / GDS… ci-dessus)."],
    ["filter", "거르기", "絞り込み", "筛选", "filtrar", "Filter", "filtrer"],
    ["layer map: {}", "레이어 맵: {}", "レイヤマップ: {}", "层映射：{}", "mapa de capas: {}", "Lagenzuordnung: {}", "table des couches : {}"],
    ["{} ports · {} transistors", "포트 {}개 · 트랜지스터 {}개", "ポート {} 個 · トランジスタ {} 個", "{} 个端口 · {} 个晶体管", "{} puertos · {} transistores", "{} Ports · {} Transistoren", "{} ports · {} transistors"],
    ["Extraction rule errors", "추출 규칙 오류", "抽出ルールのエラー", "提取规则错误", "Errores de las reglas de extracción", "Fehler in den Extraktionsregeln", "Erreurs des règles d'extraction"],
    ["Extraction warnings", "추출 경고", "抽出の警告", "提取警告", "Avisos de extracción", "Extraktionswarnungen", "Avertissements d'extraction"],
    ["CDL devices without a layout match", "레이아웃과 대응하지 않는 CDL 장치", "レイアウトと対応しない CDL デバイス", "版图中没有对应的 CDL 器件", "Dispositivos CDL sin pareja en el layout", "CDL-Bauteile ohne Gegenstück im Layout", "Composants CDL sans correspondance dans le layout"],
    ["Click a transistor or a net, in the schematic or in the layout.", "회로도나 레이아웃에서 트랜지스터나 넷을 누르세요.", "回路図かレイアウトで、トランジスタかネットをクリックしてください。", "请在原理图或版图中点击晶体管或线网。", "Haz clic en un transistor o una red, en el esquema o en el layout.", "Auf einen Transistor oder ein Netz klicken, im Schaltplan oder im Layout.", "Cliquez sur un transistor ou un net, dans le schéma ou dans le layout."],
    ["no layout fingers matched", "대응하는 레이아웃 finger 없음", "対応するレイアウトのフィンガーなし", "版图中没有对应的 finger", "ningún finger del layout emparejado", "keine Layout-Finger zugeordnet", "aucun finger du layout apparié"],
    ["layout: {} finger{}, {} fins (highlighted)", "레이아웃: finger {0}개, fin {2}개 (강조 표시)", "レイアウト: フィンガー {0} 本、フィン {2} 本（強調表示）", "版图：{0} 个 finger，{2} 个 fin（已高亮）", "layout: {0} finger{1}, {2} fins (resaltados)", "Layout: {0} Finger, {2} Fins (hervorgehoben)", "layout : {0} finger{1}, {2} fins (en surbrillance)"],
    ["layout: {} shapes on {} (highlighted)", "레이아웃: {1} 위 도형 {0}개 (강조 표시)", "レイアウト: {1} 上の図形 {0} 個（強調表示）", "版图：{1} 上的 {0} 个图形（已高亮）", "layout: {} formas en {} (resaltadas)", "Layout: {} Formen auf {} (hervorgehoben)", "layout : {} formes sur {} (en surbrillance)"],
    ["no layout shapes for this net (an internal node inside merged diffusion, or no match)", "이 넷의 레이아웃 도형 없음 (합쳐진 확산 영역 안의 내부 노드이거나 대응 없음)", "このネットのレイアウト図形なし（結合した拡散領域内の内部ノード、または対応なし）", "此线网没有版图图形（合并扩散区中的内部节点，或没有对应）", "no hay formas del layout para esta red (un nodo interno dentro de una difusión fusionada, o sin pareja)", "keine Layoutformen für dieses Netz (ein interner Knoten in zusammengelegter Diffusion, oder keine Zuordnung)", "aucune forme du layout pour ce net (un nœud interne dans une diffusion fusionnée, ou aucune correspondance)"],
    ["layout net on {}", "레이아웃 넷 ({})", "レイアウトのネット（{}）", "版图线网，位于 {}", "red del layout en {}", "Layout-Netz auf {}", "net du layout sur {}"],
    ["layout only: no CDL net matched", "레이아웃에만 있음: 대응하는 CDL 넷 없음", "レイアウトのみ: 対応する CDL ネットなし", "仅在版图中：没有对应的 CDL 线网", "solo en el layout: ninguna red CDL emparejada", "nur im Layout: kein CDL-Netz zugeordnet", "seulement dans le layout : aucun net CDL apparié"],
    ["layout only: no CDL device matched", "레이아웃에만 있음: 대응하는 CDL 장치 없음", "レイアウトのみ: 対応する CDL デバイスなし", "仅在版图中：没有对应的 CDL 器件", "solo en el layout: ningún dispositivo CDL emparejado", "nur im Layout: kein CDL-Bauteil zugeordnet", "seulement dans le layout : aucun composant CDL apparié"],
    ["GDS layers", "GDS 레이어", "GDS レイヤ", "GDS 层", "Capas GDS", "GDS-Lagen", "Couches GDS"],
    ["Extraction rules", "추출 규칙", "抽出ルール", "提取规则", "Reglas de extracción", "Extraktionsregeln", "Règles d'extraction"],
    ["Apply", "적용", "適用", "应用", "Aplicar", "Anwenden", "Appliquer"],
    ["ASAP7 defaults", "ASAP7 기본값", "ASAP7 の既定値", "ASAP7 默认值", "Valores de ASAP7", "ASAP7-Standard", "Valeurs ASAP7"],
    ["schematic", "회로도", "回路図", "原理图", "esquema", "Schaltplan", "schéma"],
    ["open a CDL or LPE SPICE netlist", "CDL 또는 LPE SPICE 넷리스트를 여세요", "CDL か LPE SPICE ネットリストを開いてください", "请打开 CDL 或 LPE SPICE 网表", "abre una netlist CDL o SPICE LPE", "CDL- oder LPE-SPICE-Netzliste öffnen", "ouvrez une netlist CDL ou SPICE LPE"],
    ["layout", "레이아웃", "レイアウト", "版图", "layout", "Layout", "layout"],
    ["open a GDS file", "GDS 파일을 여세요", "GDS ファイルを開いてください", "请打开 GDS 文件", "abre un archivo GDS", "GDS-Datei öffnen", "ouvrez un fichier GDS"],
    ["open a cell LEF", "셀 LEF를 여세요", "セル LEF を開いてください", "请打开单元 LEF", "abre un LEF de celdas", "Zell-LEF öffnen", "ouvrez un LEF de cellules"],
    ["it is not in the LEF", "LEF에 없어요", "LEF にありません", "LEF 中没有", "no está en el LEF", "nicht im LEF", "absent du LEF"],
    ["LEF abstract", "LEF 추상 뷰", "LEF アブストラクト", "LEF 抽象视图", "abstract LEF", "LEF-Abstract", "abstract LEF"],
    ["{} has no {}: {}", "{0}: {1} 없음 ({2})", "{0} には{1}がありません: {2}", "{0} 没有{1}：{2}", "{0} no tiene {1}: {2}", "{0}: {1} fehlt ({2})", "{0} : {1} absent ({2})"],
    ["Drop a CDL netlist and a GDS file here", "CDL 넷리스트와 GDS 파일을 여기에 놓으세요", "ここに CDL ネットリストと GDS ファイルをドロップ", "把 CDL 网表和 GDS 文件拖到这里", "Suelta aquí una netlist CDL y un archivo GDS", "CDL-Netzliste und GDS-Datei hier ablegen", "Déposez ici une netlist CDL et un fichier GDS"],
    ["{}  ·  {} × {} µm  ·  {} pins  ·  {} obstruction shapes", "{}  ·  {} × {} µm  ·  핀 {}개  ·  차단 도형 {}개", "{}  ·  {} × {} µm  ·  ピン {} 個  ·  配線禁止図形 {} 個", "{}  ·  {} × {} µm  ·  {} 个引脚  ·  {} 个阻挡图形", "{}  ·  {} × {} µm  ·  {} pines  ·  {} formas de obstrucción", "{}  ·  {} × {} µm  ·  {} Pins  ·  {} Sperrflächen", "{}  ·  {} × {} µm  ·  {} broches  ·  {} formes d'obstruction"],
    ["pin {} ({})  ·  {}", "핀 {} ({})  ·  {}", "ピン {} ({})  ·  {}", "引脚 {} ({})  ·  {}", "pin {} ({})  ·  {}", "Pin {} ({})  ·  {}", "broche {} ({})  ·  {}"],
    // parasitics pane
    ["{} (one end)", "{} (한쪽 끝)", "{}（片端）", "{}（一端）", "{} (un extremo)", "{} (ein Ende)", "{} (une extrémité)"],
    ["No cell yet.", "아직 셀이 없어요.", "まだセルがありません。", "还没有单元。", "Todavía no hay ninguna celda.", "Noch keine Zelle.", "Pas encore de cellule."],
    ["Open an LPE (post-layout) SPICE netlist with File > Cell > Open CDL / SPICE / LPE netlist… to list its resistors and capacitors here.", "파일 > 셀 > CDL / SPICE / LPE 넷리스트 열기…로 LPE(레이아웃 후) SPICE 넷리스트를 열면 저항과 커패시터가 여기에 나와요.", "ファイル > セル > CDL / SPICE / LPE ネットリストを開く… で LPE（レイアウト後）SPICE ネットリストを開くと、抵抗と容量がここに一覧表示されます。", "用 文件 > 单元 > 打开 CDL / SPICE / LPE 网表… 打开 LPE（版图后）SPICE 网表，这里会列出其中的电阻和电容。", "Abre una netlist SPICE LPE (post-layout) con Archivo > Celda > Abrir netlist CDL / SPICE / LPE… para ver aquí sus resistencias y condensadores.", "Eine LPE-SPICE-Netzliste (nach dem Layout) mit Datei > Zelle > CDL-/SPICE-/LPE-Netzliste öffnen… laden, um hier ihre Widerstände und Kapazitäten zu sehen.", "Ouvrez une netlist SPICE LPE (post-layout) avec Fichier > Cellule > Ouvrir une netlist CDL / SPICE / LPE… pour voir ici ses résistances et capacités."],
    ["{} is not in {}. Cells there: {}", "{0}: {1}에 없어요. 그 파일의 셀: {2}", "{0} は {1} にありません。そこにあるセル: {2}", "{1} 中没有 {0}。其中的单元：{2}", "{} no está en {}. Celdas que contiene: {}", "{} ist nicht in {}. Enthaltene Zellen: {}", "{} n'est pas dans {}. Cellules présentes : {}"],
    ["{} R · {} C · {} coupling C · {}", "R {}개 · C {}개 · 커플링 C {}개 · {}", "R {} 個 · C {} 個 · カップリング C {} 個 · {}", "{} 个 R · {} 个 C · {} 个耦合 C · {}", "{} R · {} C · {} C de acoplamiento · {}", "{} R · {} C · {} Koppel-C · {}", "{} R · {} C · {} C de couplage · {}"],
    ["No layers: the GDS of this cell is needed (and its LVS must match) to place the parasitics on layers and segments.", "레이어 정보 없음: 기생 성분을 레이어와 세그먼트에 놓으려면 이 셀의 GDS가 있어야 하고 LVS가 일치해야 해요.", "レイヤ情報なし: 寄生成分をレイヤとセグメントに割り当てるには、このセルの GDS が必要で、LVS が一致している必要があります。", "没有层信息：要把寄生参数归到层和线段上，需要此单元的 GDS，并且 LVS 必须一致。", "Sin capas: hace falta el GDS de esta celda (y que su LVS coincida) para situar los parásitos en capas y segmentos.", "Keine Lagen: Um die parasitären Elemente Lagen und Segmenten zuzuordnen, wird das GDS dieser Zelle benötigt (und ihr LVS muss stimmen).", "Pas de couches : il faut le GDS de cette cellule (et un LVS qui correspond) pour placer les parasites sur les couches et les segments."],
    ["Totals by layer", "레이어별 합계", "レイヤ別の合計", "按层合计", "Totales por capa", "Summen je Lage", "Totaux par couche"],
    ["layer", "레이어", "レイヤ", "层", "capa", "Lage", "couche"],
    ["ΣC to ground", "ΣC (접지)", "ΣC（対グラウンド）", "ΣC（对地）", "ΣC a tierra", "ΣC gegen Masse", "ΣC vers la masse"],
    ["ΣCc (half per side)", "ΣCc (양쪽에 절반씩)", "ΣCc（両側に半分ずつ）", "ΣCc（每侧一半）", "ΣCc (mitad por lado)", "ΣCc (je Seite die Hälfte)", "ΣCc (moitié par côté)"],
    ["unknown", "알 수 없음", "不明", "未知", "desconocida", "unbekannt", "inconnue"],
    ["{} nodes sit on a via stack with no way to tell the layer from the netlist: they take the top layer of the stack (marked * below).", "노드 {}개는 비아 스택 위에 있어서 넷리스트만으로는 레이어를 알 수 없어요. 스택의 맨 위 레이어로 정했어요 (아래에 * 표시).", "{} 個のノードはビアスタック上にあり、ネットリストからはレイヤを判別できません。スタックの最上位レイヤとしています（下で * 印）。", "有 {} 个节点位于通孔堆叠上，无法从网表判断其层：它们取堆叠的最上层（下方以 * 标出）。", "{} nodos están sobre una pila de vías y la netlist no permite saber su capa: se les asigna la capa superior de la pila (marcados con * abajo).", "{} Knoten liegen auf einem Via-Stapel, deren Lage sich aus der Netzliste nicht bestimmen lässt: Sie erhalten die oberste Lage des Stapels (unten mit * markiert).", "{} nœuds sont sur une pile de vias sans moyen de connaître leur couche depuis la netlist : ils prennent la couche du haut de la pile (marqués * ci-dessous)."],
    ["By element", "소자별", "素子別", "按元件", "Por elemento", "Nach Element", "Par élément"],
    ["By segment", "세그먼트별", "セグメント別", "按线段", "Por segmento", "Nach Segment", "Par segment"],
    ["Coupling C", "커플링 C", "カップリング C", "耦合 C", "C de acoplamiento", "Koppel-C", "C de couplage"],
    ["Segments with no R or C", "R, C 없는 세그먼트도", "R も C もないセグメント", "无 R 或 C 的线段", "Segmentos sin R ni C", "Segmente ohne R oder C", "Segments sans R ni C"],
    ["net, layer or segment", "넷, 레이어, 세그먼트", "ネット、レイヤ、セグメント", "线网、层或线段", "red, capa o segmento", "Netz, Lage oder Segment", "net, couche ou segment"],
    ["Dots on the layout", "레이아웃에 점 표시", "レイアウトに点を表示", "在版图上显示点", "Puntos en el layout", "Punkte im Layout", "Points sur le layout"],
    ["side A", "A쪽", "A 側", "A 侧", "lado A", "Seite A", "côté A"],
    ["side B of a coupling C", "커플링 C의 B쪽", "カップリング C の B 側", "耦合 C 的 B 侧", "lado B de una C de acoplamiento", "Seite B eines Koppel-C", "côté B d'une C de couplage"],
    // a monospace row: only "via R" changes, CJK keeps it so the columns stay aligned
    ["{:<22} {:.3} x {:.3} µm   R {:>2} {:>11}   via R {:>2} {:>11}   C {:>11}   Cc {:>3} {:>11}", "", "", "", "{:<22} {:.3} x {:.3} µm   R {:>2} {:>11}   R vía {:>2} {:>11}   C {:>11}   Cc {:>3} {:>11}", "{:<22} {:.3} x {:.3} µm   R {:>2} {:>11}   Via-R {:>2} {:>11}   C {:>11}   Cc {:>3} {:>11}", "{:<22} {:.3} x {:.3} µm   R {:>2} {:>11}   R via {:>2} {:>11}   C {:>11}   Cc {:>3} {:>11}"],
    ["{} shown, largest first{}", "{}개 표시, 큰 것부터{}", "{} 件を表示、大きい順{}", "显示 {} 项，从大到小{}", "{} mostrados, de mayor a menor{}", "{} angezeigt, größte zuerst{}", "{} affichés, du plus grand au plus petit{}"],
    [" (by total C)", " (C 합계 기준)", "（C の合計で）", "（按 C 总和）", " (por C total)", " (nach Gesamt-C)", " (selon la C totale)"],
    ["{}: {} between {} and {} · segment {}", "{0}: {1} ({2} - {3} 사이) · 세그먼트 {4}", "{0}: {1}（{2} と {3} の間） · セグメント {4}", "{0}：{1}，在 {2} 与 {3} 之间 · 线段 {4}", "{}: {} entre {} y {} · segmento {}", "{}: {} zwischen {} und {} · Segment {}", "{} : {} entre {} et {} · segment {}"],
    ["ground", "접지", "グラウンド", "地", "tierra", "Masse", "masse"],
    ["segment {}: {} R, {} capacitor sides (click one to show it)", "세그먼트 {}: R {}개, 커패시터 단자 {}개 (누르면 보여요)", "セグメント {}: R {} 個、容量の端 {} 個（クリックで表示）", "线段 {}：{} 个 R，{} 个电容端（点击可查看）", "segmento {}: {} R, {} lados de condensador (haz clic en uno para verlo)", "Segment {}: {} R, {} Kondensatorseiten (zum Anzeigen anklicken)", "segment {} : {} R, {} côtés de condensateur (cliquez pour l'afficher)"],
    ["Expand", "펼치기", "展開", "展开", "Expandir", "Aufklappen", "Déplier"],
    ["Collapse", "접기", "折りたたむ", "折叠", "Contraer", "Zuklappen", "Replier"],
    ["group taken from instance names", "인스턴스 이름에서 만든 묶음", "インスタンス名から作ったグループ", "由实例名得到的分组", "grupo tomado de los nombres de instancia", "aus Instanznamen gebildete Gruppe", "groupe tiré des noms d'instance"],
    ["module {}", "모듈 {}", "モジュール {}", "模块 {}", "módulo {}", "Modul {}", "module {}"],
    ["{} cells, {} submodules, {} pins", "셀 {}개, 하위 모듈 {}개, 핀 {}개", "セル {} 個、サブモジュール {} 個、ピン {} 個", "{} 个单元，{} 个子模块，{} 个引脚", "{} celdas, {} submódulos, {} pines", "{} Zellen, {} Untermodule, {} Pins", "{} cellules, {} sous-modules, {} broches"],
    ["in {} (placed by its connections)", "{} 안 (연결로 배치)", "{} 内（接続から配置）", "在 {} 中（按连接归入）", "en {} (ubicada por sus conexiones)", "in {} (über die Verbindungen zugeordnet)", "dans {} (placée d'après ses connexions)"],
    ["in {}", "{} 안", "{} 内", "在 {} 中", "en {}", "in {}", "dans {}"],
    ["{}  ·  {} cells", "{}  ·  셀 {}개", "{}  ·  セル {} 個", "{}  ·  {} 个单元", "{}  ·  {} celdas", "{}  ·  {} Zellen", "{}  ·  {} cellules"],
    ["{}  ·  {}  ·  {} cells", "{}  ·  {}  ·  셀 {}개", "{}  ·  {}  ·  セル {} 個", "{}  ·  {}  ·  {} 个单元", "{}  ·  {}  ·  {} celdas", "{}  ·  {}  ·  {} Zellen", "{}  ·  {}  ·  {} cellules"],
    ["{} cells", "셀 {}개", "セル {} 個", "{} 个单元", "{} celdas", "{} Zellen", "{} cellules"],
    ["{} holds {} cells and submodules directly: too many for one sheet (Around the selection shows them).", "{0} 바로 아래에 셀과 하위 모듈이 {1}개 있어서 한 화면에 그리기에는 너무 많아요. 선택 주변 보기로 볼 수 있어요.", "{0} の直下にはセルとサブモジュールが {1} 個あり、1 枚に描くには多すぎます。選択の周辺で表示できます。", "{0} 直接包含 {1} 个单元和子模块，一张图放不下。可用“所选对象周围”查看。", "{} contiene {} celdas y submódulos directamente: demasiados para una hoja (Alrededor de la selección los muestra).", "{} enthält direkt {} Zellen und Untermodule: zu viele für ein Blatt (Um die Auswahl zeigt sie).", "{} contient directement {} cellules et sous-modules : trop pour une feuille (Autour de la sélection les montre)."],
    ["Laying out the netlist view…", "넷리스트 화면 배치 중…", "ネットリスト表示を配置中…", "正在布局网表视图…", "Colocando la vista del netlist…", "Netzlistenansicht wird angeordnet…", "Placement de la vue netlist…"],
    ["Netlist: {} cells of {} drawn, {} submodules closed, {} open | layout {} ms", "넷리스트: 셀 {1}개 중 {0}개 그림, 하위 모듈 닫힘 {2}개, 열림 {3}개 | 배치 {4} ms", "ネットリスト: セル {1} 個中 {0} 個を描画、サブモジュール 閉 {2} 個、開 {3} 個 | 配置 {4} ms", "网表：已绘制 {1} 个单元中的 {0} 个，{2} 个子模块折叠，{3} 个展开 | 布局 {4} ms", "Netlist: {} de {} celdas dibujadas, {} submódulos cerrados, {} abiertos | colocación {} ms", "Netzliste: {} von {} Zellen gezeichnet, {} Untermodule zu, {} offen | Anordnung {} ms", "Netlist : {} cellules sur {} dessinées, {} sous-modules fermés, {} ouverts | placement {} ms"],
    ["Netlist: {} cells of {} drawn (whole design) | layout {} ms", "넷리스트: 셀 {1}개 중 {0}개 그림 (전체 설계) | 배치 {2} ms", "ネットリスト: セル {1} 個中 {0} 個を描画（設計全体） | 配置 {2} ms", "网表：已绘制 {1} 个单元中的 {0} 个（整个设计） | 布局 {2} ms", "Netlist: {} de {} celdas dibujadas (diseño completo) | colocación {} ms", "Netzliste: {} von {} Zellen gezeichnet (ganzer Entwurf) | Anordnung {} ms", "Netlist : {} cellules sur {} dessinées (conception entière) | placement {} ms"],
    ["the CDL lost a transistor to a malformed line: {}", "CDL에서 잘못된 줄 때문에 트랜지스터 하나가 빠졌어요: {}", "CDL の不正な行のためトランジスタが 1 つ失われました: {}", "CDL 因格式错误的行丢失了一个晶体管：{}", "el CDL perdió un transistor por una línea mal formada: {}", "die CDL hat durch eine fehlerhafte Zeile einen Transistor verloren: {}", "le CDL a perdu un transistor à cause d'une ligne mal formée : {}"],
    ["the CDL subcircuit has no transistors the reader accepted", "CDL 서브서킷에 읽을 수 있는 트랜지스터가 없어요", "CDL サブサーキットに読み込めたトランジスタがありません", "CDL 子电路中没有可读入的晶体管", "el subcircuito CDL no tiene transistores que el lector aceptara", "der CDL-Teilschaltkreis hat keine Transistoren, die der Leser akzeptiert hat", "le sous-circuit CDL n'a aucun transistor accepté par le lecteur"],
    ["pin text {} has no CDL port of that name (the CDL port is {}, names are compared exactly)", "핀 글자 {0}: 같은 이름의 CDL 포트가 없어요 (CDL 포트는 {1}, 이름은 글자 그대로 비교해요)", "ピン文字 {0} と同じ名前の CDL ポートがありません（CDL ポートは {1}、名前は完全一致で比較）", "引脚文字 {0} 没有同名的 CDL 端口（CDL 端口是 {1}，名称按完全一致比较）", "el texto de pin {} no tiene un puerto CDL con ese nombre (el puerto CDL es {}, los nombres se comparan exactamente)", "zum Pin-Text {} gibt es keinen CDL-Port dieses Namens (der CDL-Port heißt {}, Namen werden genau verglichen)", "le texte de broche {} n'a pas de port CDL de ce nom (le port CDL est {}, les noms sont comparés exactement)"],
    ["pin text {} has no CDL port of that name", "핀 글자 {}: 같은 이름의 CDL 포트가 없어요", "ピン文字 {} と同じ名前の CDL ポートがありません", "引脚文字 {} 没有同名的 CDL 端口", "el texto de pin {} no tiene un puerto CDL con ese nombre", "zum Pin-Text {} gibt es keinen CDL-Port dieses Namens", "le texte de broche {} n'a pas de port CDL de ce nom"],
    ["pin text {} is on a net with no devices (its shapes are only on {}, nothing connects them down to a gate or diffusion)", "핀 글자 {0}: 소자가 없는 넷에 있어요 (도형이 {1}에만 있고 게이트나 확산으로 내려가는 연결이 없어요)", "ピン文字 {0} はデバイスのないネット上にあります（図形は {1} のみで、ゲートや拡散へ下りる接続がありません）", "引脚文字 {0} 位于没有器件的线网上（图形只在 {1} 上，没有向下连到栅极或扩散区）", "el texto de pin {} está en una red sin dispositivos (sus formas solo están en {}, nada las conecta con una puerta o difusión)", "Pin-Text {} liegt auf einem Netz ohne Bauelemente (seine Formen liegen nur auf {}, nichts verbindet sie nach unten mit Gate oder Diffusion)", "le texte de broche {} est sur un réseau sans composants (ses formes ne sont que sur {}, rien ne les relie à une grille ou une diffusion)"],
    ["pin text {} is on a diffusion (source/drain) net, but CDL port {} connects only to gates", "핀 글자 {0}: 확산(소스/드레인) 넷에 있는데, CDL 포트 {1}의 연결은 게이트뿐이에요", "ピン文字 {0} は拡散（ソース/ドレイン）ネット上にありますが、CDL ポート {1} はゲートにのみ接続します", "引脚文字 {0} 位于扩散区（源/漏）线网上，但 CDL 端口 {1} 只连接栅极", "el texto de pin {} está en una red de difusión (fuente/drenador), pero el puerto CDL {} solo conecta con puertas", "Pin-Text {} liegt auf einem Diffusionsnetz (Source/Drain), aber der CDL-Port {} geht nur an Gates", "le texte de broche {} est sur un réseau de diffusion (source/drain), mais le port CDL {} ne va qu'à des grilles"],
    ["pin text {} is on a gate net, but CDL port {} connects only to source/drain", "핀 글자 {0}: 게이트 넷에 있는데, CDL 포트 {1}의 연결은 소스/드레인뿐이에요", "ピン文字 {0} はゲートネット上にありますが、CDL ポート {1} はソース/ドレインにのみ接続します", "引脚文字 {0} 位于栅极线网上，但 CDL 端口 {1} 只连接源/漏", "el texto de pin {} está en una red de puerta, pero el puerto CDL {} solo conecta con fuente/drenador", "Pin-Text {} liegt auf einem Gate-Netz, aber der CDL-Port {} geht nur an Source/Drain", "le texte de broche {} est sur un réseau de grille, mais le port CDL {} ne va qu'à source/drain"],
    ["CDL port {} has no pin text in the layout", "CDL 포트 {}: 레이아웃에 핀 글자가 없어요", "CDL ポート {} にはレイアウト上のピン文字がありません", "CDL 端口 {} 在版图中没有引脚文字", "el puerto CDL {} no tiene texto de pin en el layout", "der CDL-Port {} hat im Layout keinen Pin-Text", "le port CDL {} n'a pas de texte de broche dans le layout"],
    ["pin text {} and CDL port {} are the only unpaired names: the same pin may have two names", "짝이 없는 이름은 핀 글자 {0}, CDL 포트 {1} 둘뿐이에요. 같은 핀이 이름 두 개를 가졌을 수 있어요", "対応のない名前はピン文字 {0} と CDL ポート {1} だけです。同じピンに 2 つの名前があるのかもしれません", "只有引脚文字 {0} 和 CDL 端口 {1} 没有配对：同一个引脚可能有两个名字", "el texto de pin {} y el puerto CDL {} son los únicos nombres sin pareja: el mismo pin puede tener dos nombres", "Pin-Text {} und CDL-Port {} sind die einzigen Namen ohne Partner: derselbe Pin hat vielleicht zwei Namen", "le texte de broche {} et le port CDL {} sont les seuls noms sans paire : la même broche a peut-être deux noms"],
    [" | {} transistors in symmetric copies, paired by order", " | 대칭 복사본 트랜지스터 {}개, 순서로 짝지음", " | 対称なコピーのトランジスタ {} 個、順番で対応付け", " | {} 个晶体管属于对称副本，按顺序配对", " | {} transistores en copias simétricas, emparejados por orden", " | {} Transistoren in symmetrischen Kopien, nach Reihenfolge zugeordnet", " | {} transistors dans des copies symétriques, appariés par ordre"],
    [" | likely cause: {}", " | 가능한 원인: {}", " | 考えられる原因: {}", " | 可能的原因：{}", " | causa probable: {}", " | wahrscheinliche Ursache: {}", " | cause probable : {}"],
    ["Symmetric: {} transistors have interchangeable layout copies (nets {}). Every pairing of the copies passes. The one shown pairs CDL order with layout left to right.", "대칭: 트랜지스터 {0}개에 서로 바꿔도 맞는 레이아웃 복사본이 있어요 (넷 {1}). 어떻게 짝지어도 통과해요. 보이는 짝은 CDL 순서와 레이아웃 왼쪽부터의 순서로 정했어요.", "対称: トランジスタ {0} 個に入れ替えても一致するレイアウトのコピーがあります（ネット {1}）。どの対応付けでも通ります。表示中の対応は CDL の順番とレイアウトの左からの順番で決めています。", "对称：{0} 个晶体管有可互换的版图副本（线网 {1}）。任何配对方式都能通过。显示的配对按 CDL 顺序与版图从左到右的顺序确定。", "Simétrico: {} transistores tienen copias intercambiables en el layout (redes {}). Cualquier emparejamiento de las copias pasa. El que se muestra empareja el orden del CDL con el layout de izquierda a derecha.", "Symmetrisch: {} Transistoren haben austauschbare Layout-Kopien (Netze {}). Jede Zuordnung der Kopien passt. Die gezeigte ordnet die CDL-Reihenfolge dem Layout von links nach rechts zu.", "Symétrique : {} transistors ont des copies interchangeables dans le layout (réseaux {}). Tout appariement des copies passe. Celui affiché associe l'ordre du CDL au layout de gauche à droite."],
    ["cause: {}", "원인: {}", "原因: {}", "原因：{}", "causa: {}", "Ursache: {}", "cause : {}"],
    ["symmetric copy: another layout copy matches as well, this one is paired by order", "대칭 복사본: 다른 레이아웃 복사본도 맞아요. 이것은 순서로 짝지었어요", "対称なコピー: 別のレイアウトのコピーも一致します。これは順番で対応付けています", "对称副本：另一个版图副本同样匹配，这个是按顺序配对的", "copia simétrica: otra copia del layout también coincide, esta se empareja por orden", "symmetrische Kopie: eine andere Layout-Kopie passt ebenso, diese ist nach Reihenfolge zugeordnet", "copie symétrique : une autre copie du layout correspond aussi, celle-ci est appariée par ordre"],
    ["Reading notes ({})", "읽기 메모 ({})", "読み込みメモ ({})", "读取说明 ({})", "Notas de lectura ({})", "Lesehinweise ({})", "Notes de lecture ({})"],
    ["{} R take the layer of R placed by position in this cell whose value they equal, not their own position (marked = below).", "R {}개는 자기 위치가 아니라, 이 셀에서 위치로 정해진 R 중 값이 같은 R의 레이어를 따라요 (아래에 = 표시).", "R {} 個は自分の位置ではなく、このセルで位置から決まった R のうち値が等しいもののレイヤーを使います（下の = 印）。", "{} 个 R 不按自身位置，而是采用本单元中按位置确定且阻值相同的 R 的层（下方以 = 标记）。", "{} R toman la capa de las R ubicadas por posición en esta celda cuyo valor igualan, no su propia posición (marcadas con = abajo).", "{} R übernehmen die Lage der per Position platzierten R dieser Zelle mit gleichem Wert, nicht ihre eigene Position (unten mit = markiert).", "{} R prennent la couche des R placées par position dans cette cellule dont elles ont la même valeur, pas leur propre position (marquées = ci-dessous)."],
    ["Reading and parsing {} on {} threads", "{0} 읽고 해석하는 중 (스레드 {1}개)", "{0} を読み込み・解析中（スレッド {1} 個）", "正在读取并解析 {0}（{1} 个线程）", "Leyendo y analizando {} con {} hilos", "{} wird mit {} Threads gelesen und ausgewertet", "Lecture et analyse de {} sur {} threads"],
    ["{}[{}..{}] ({} bits)", "{}[{}..{}] ({}비트)", "{}[{}..{}]（{} ビット）", "{}[{}..{}]（{} 位）", "{}[{}..{}] ({} bits)", "{}[{}..{}] ({} Bits)", "{}[{}..{}] ({} bits)"],
    ["{} nets", "넷 {}개", "ネット {} 本", "{} 个线网", "{} redes", "{} Netze", "{} réseaux"],
    ["bundle {}", "묶음 {}", "束 {}", "线束 {}", "haz {}", "Bündel {}", "faisceau {}"],
    ["bundle of {} nets", "넷 {}개 묶음", "ネット {} 本の束", "{} 个线网的线束", "haz de {} redes", "Bündel aus {} Netzen", "faisceau de {} réseaux"],
    ["{}  ·  {} nets", "{}  ·  넷 {}개", "{}  ·  ネット {} 本", "{}  ·  {} 个线网", "{}  ·  {} redes", "{}  ·  {} Netze", "{}  ·  {} réseaux"],
    ["Logic check", "논리 검사", "論理チェック", "逻辑检查", "Comprobación lógica", "Logikprüfung", "Vérification logique"],
    ["Needs the transistors of the cell: a CDL or post-layout netlist, or a GDS.", "셀의 트랜지스터가 있어야 해요. CDL이나 레이아웃 후 넷리스트, 또는 GDS를 여세요.", "セルのトランジスタが必要です。CDL かレイアウト後のネットリスト、または GDS を開いてください。", "需要单元的晶体管：请打开 CDL 或版图后网表，或 GDS。", "Hacen falta los transistores de la celda: una netlist CDL o post-layout, o un GDS.", "Die Transistoren der Zelle werden gebraucht: eine CDL- oder Post-Layout-Netzliste oder ein GDS.", "Il faut les transistors de la cellule : une netlist CDL ou post-layout, ou un GDS."],
    ["The function of each output as in Liberty, one per line, for example Y = !(A & B). Every input vector is simulated on the transistors.", "출력마다 Liberty처럼 논리식을 한 줄에 하나씩 쓰세요. 예: Y = !(A & B). 모든 입력 벡터를 트랜지스터로 시뮬레이션해요.", "出力ごとの論理式を Liberty と同じ書き方で 1 行に 1 つ書いてください。例: Y = !(A & B)。すべての入力ベクトルをトランジスタでシミュレーションします。", "按 Liberty 的写法每行写一个输出的函数，例如 Y = !(A & B)。所有输入向量都在晶体管上仿真。", "La función de cada salida como en Liberty, una por línea, por ejemplo Y = !(A & B). Se simula cada vector de entrada sobre los transistores.", "Die Funktion jedes Ausgangs wie in Liberty, eine pro Zeile, zum Beispiel Y = !(A & B). Jeder Eingangsvektor wird auf den Transistoren simuliert.", "La fonction de chaque sortie comme dans Liberty, une par ligne, par exemple Y = !(A & B). Chaque vecteur d'entrée est simulé sur les transistors."],
    ["Check", "검사", "チェック", "检查", "Comprobar", "Prüfen", "Vérifier"],
    ["From Liberty", "Liberty에서 가져오기", "Liberty から", "从 Liberty 读取", "Desde Liberty", "Aus Liberty", "Depuis Liberty"],
    ["Liberty: {}", "Liberty: {}", "Liberty: {}", "Liberty：{}", "Liberty: {}", "Liberty: {}", "Liberty : {}"],
    ["only differing vectors", "다른 벡터만", "異なるベクトルのみ", "仅显示不同的向量", "solo vectores distintos", "nur abweichende Vektoren", "seulement les vecteurs différents"],
    ["{}: all {} input vectors match", "{}: 입력 벡터 {}개 모두 일치", "{}: 入力ベクトル {} 個すべて一致", "{}：全部 {} 个输入向量一致", "{}: coinciden los {} vectores de entrada", "{}: alle {} Eingangsvektoren stimmen", "{} : les {} vecteurs d'entrée concordent"],
    ["{}: {} of {} input vectors differ", "{0}: 입력 벡터 {2}개 중 {1}개가 달라요", "{0}: 入力ベクトル {2} 個中 {1} 個が異なります", "{0}：{2} 个输入向量中有 {1} 个不同", "{}: {} de {} vectores de entrada difieren", "{}: {} von {} Eingangsvektoren weichen ab", "{} : {} vecteurs d'entrée sur {} diffèrent"],
    ["Not in the functions, left floating: {}", "논리식에 없어서 떠 있는 핀: {}", "論理式になく浮いたままのピン: {}", "函数中没有、保持悬空的引脚：{}", "Fuera de las funciones, quedan flotando: {}", "Nicht in den Funktionen, bleiben offen: {}", "Absents des fonctions, laissés flottants : {}"],
    ["{} of {} rows shown", "{1}행 중 {0}행 표시", "{1} 行中 {0} 行を表示", "显示 {1} 行中的 {0} 行", "{} de {} filas mostradas", "{} von {} Zeilen gezeigt", "{} lignes sur {} affichées"],
    ["post-layout netlist", "레이아웃 후 넷리스트", "レイアウト後ネットリスト", "版图后网表", "netlist post-layout", "Post-Layout-Netzliste", "netlist post-layout"],
    ["'{}' has no = (write OUTPUT = function)", "'{}'에 =가 없어요 (출력 = 논리식으로 쓰세요)", "'{}' に = がありません（出力 = 論理式 と書いてください）", "'{}' 中没有 =（请写成 输出 = 函数）", "'{}' no tiene = (escribe SALIDA = función)", "'{}' hat kein = (AUSGANG = Funktion schreiben)", "'{}' n'a pas de = (écrivez SORTIE = fonction)"],
    ["write three_state(OUTPUT) = function, not '{}'", "'{}' 말고 three_state(출력) = 논리식으로 쓰세요", "'{}' ではなく three_state(出力) = 論理式 と書いてください", "请写成 three_state(输出) = 函数，而不是 '{}'", "escribe three_state(SALIDA) = función, no '{}'", "three_state(AUSGANG) = Funktion schreiben, nicht '{}'", "écrivez three_state(SORTIE) = fonction, pas '{}'"],
    ["'{}' is not a pin name", "'{}'는 핀 이름이 아니에요", "'{}' はピン名ではありません", "'{}' 不是引脚名", "'{}' no es un nombre de pin", "'{}' ist kein Pinname", "'{}' n'est pas un nom de broche"],
    ["{} is given twice", "{}가 두 번 있어요", "{} が 2 回書かれています", "{} 写了两次", "{} aparece dos veces", "{} steht zweimal da", "{} est donné deux fois"],
    ["three_state({}) names no output with a function", "three_state({})가 가리키는 출력에 논리식이 없어요", "three_state({}) の出力に論理式がありません", "three_state({}) 指向的输出没有函数", "three_state({}) no nombra ninguna salida con función", "three_state({}) nennt keinen Ausgang mit Funktion", "three_state({}) ne désigne aucune sortie avec une fonction"],
    ["no function given", "논리식이 없어요", "論理式がありません", "没有给出函数", "no hay ninguna función", "keine Funktion angegeben", "aucune fonction donnée"],
    ["sequential cell (flip-flop or latch), only combinational outputs are checked", "순차 셀(플립플롭이나 래치)이에요. 조합 출력만 검사해요", "順序セル（フリップフロップかラッチ）です。組み合わせ出力だけを検査します", "时序单元（触发器或锁存器），只检查组合输出", "celda secuencial (flip-flop o latch), solo se comprueban salidas combinacionales", "sequentielle Zelle (Flipflop oder Latch), nur kombinatorische Ausgänge werden geprüft", "cellule séquentielle (bascule ou verrou), seules les sorties combinatoires sont vérifiées"],
    ["no output with a function in the Liberty file", "Liberty 파일에 논리식이 있는 출력이 없어요", "Liberty ファイルに論理式のある出力がありません", "Liberty 文件中没有带函数的输出", "ninguna salida con función en el archivo Liberty", "kein Ausgang mit Funktion in der Liberty-Datei", "aucune sortie avec une fonction dans le fichier Liberty"],
    ["holds a state (an output function names an output), only combinational outputs are checked", "상태를 기억하는 셀이에요(출력 논리식에 출력이 들어 있어요). 조합 출력만 검사해요", "状態を保持するセルです（出力の論理式に出力が含まれます）。組み合わせ出力だけを検査します", "保持状态（输出函数中含有输出），只检查组合输出", "guarda un estado (una función de salida nombra una salida), solo se comprueban salidas combinacionales", "hält einen Zustand (eine Ausgangsfunktion nennt einen Ausgang), nur kombinatorische Ausgänge werden geprüft", "garde un état (une fonction de sortie nomme une sortie), seules les sorties combinatoires sont vérifiées"],
    ["{} is not a net of this circuit", "{}는 이 회로의 넷이 아니에요", "{} はこの回路のネットではありません", "{} 不是此电路的线网", "{} no es una red de este circuito", "{} ist kein Netz dieser Schaltung", "{} n'est pas un net de ce circuit"],
    ["input {} is the supply net {}", "입력 {}가 전원 넷 {}이에요", "入力 {} は電源ネット {} です", "输入 {} 是电源线网 {}", "la entrada {} es la red de alimentación {}", "Eingang {} ist das Versorgungsnetz {}", "l'entrée {} est le net d'alimentation {}"],
    ["inputs {} and {} are one net", "입력 {}와 {}가 같은 넷이에요", "入力 {} と {} が同じネットです", "输入 {} 和 {} 是同一个线网", "las entradas {} y {} son una sola red", "Eingänge {} und {} sind ein Netz", "les entrées {} et {} sont un seul net"],
    ["no supply nets found (named VDD and VSS, VPWR and VGND or the like)", "전원 넷을 찾지 못했어요 (VDD와 VSS, VPWR와 VGND 같은 이름)", "電源ネットが見つかりません（VDD と VSS、VPWR と VGND などの名前）", "没有找到电源线网（名为 VDD 和 VSS、VPWR 和 VGND 等）", "no se encontraron redes de alimentación (llamadas VDD y VSS, VPWR y VGND o parecido)", "keine Versorgungsnetze gefunden (VDD und VSS, VPWR und VGND oder ähnlich benannt)", "aucun net d'alimentation trouvé (nommés VDD et VSS, VPWR et VGND ou semblable)"],
    ["{} is an output and an input of the functions (a stored state), only combinational functions are checked", "{}가 논리식의 출력이면서 입력이에요(기억된 상태). 조합 논리식만 검사해요", "{} は論理式の出力でもあり入力でもあります（保持された状態）。組み合わせの論理式だけを検査します", "{} 既是函数的输出又是输入（存储的状态），只检查组合函数", "{} es salida y entrada de las funciones (un estado guardado), solo se comprueban funciones combinacionales", "{} ist Ausgang und Eingang der Funktionen (ein gespeicherter Zustand), nur kombinatorische Funktionen werden geprüft", "{} est une sortie et une entrée des fonctions (un état mémorisé), seules les fonctions combinatoires sont vérifiées"],
    ["{} inputs, at most {} are checked", "입력 {}개, 최대 {}개까지 검사해요", "入力 {} 個、検査できるのは最大 {} 個です", "{} 个输入，最多检查 {} 个", "{} entradas, se comprueban como máximo {}", "{} Eingänge, höchstens {} werden geprüft", "{} entrées, au plus {} sont vérifiées"],
    ["the supplies {} are one net (a short)", "전원 {}가 한 넷이에요 (단락)", "電源 {} が同じネットです（短絡）", "电源 {} 是同一个线网（短路）", "las alimentaciones {} son una sola red (un cortocircuito)", "die Versorgungen {} sind ein Netz (ein Kurzschluss)", "les alimentations {} sont un seul net (un court-circuit)"],
    ["wire", "배선", "配線", "导线", "cable", "Leitung", "fil"],
    ["via", "비아", "ビア", "通孔", "vía", "Via", "via"],
    ["patch", "패치", "パッチ", "块", "parche", "Fläche", "zone"],
    ["path", "경로", "経路", "路径", "camino", "Pfad", "chemin"],
    ["Loading the parasitics (SPEF)…", "기생 성분(SPEF) 읽는 중…", "寄生成分 (SPEF) を読み込み中…", "正在读取寄生参数 (SPEF)…", "Cargando los parásitos (SPEF)…", "Parasitics (SPEF) werden geladen…", "Chargement des parasites (SPEF)…"],
    ["{}: {} nets, {} R, {} C ({} of {} nodes with a location)", "{}: 넷 {}개, R {}개, C {}개 (노드 {}/{}개에 위치 있음)", "{}: ネット {}、R {}、C {}（位置のあるノード {}/{}）", "{}：{} 个网络，{} 个 R，{} 个 C（{}/{} 个节点有位置）", "{}: {} redes, {} R, {} C ({} de {} nodos con posición)", "{}: {} Netze, {} R, {} C ({} von {} Knoten mit Lage)", "{} : {} réseaux, {} R, {} C ({} sur {} nœuds avec une position)"],
    ["Parasitics ({}): {} R, {} C, total {}", "기생 성분 ({}): R {}개, C {}개, 합계 {}", "寄生成分 ({}): R {}、C {}、合計 {}", "寄生参数 ({})：{} 个 R，{} 个 C，总计 {}", "Parásitos ({}): {} R, {} C, total {}", "Parasitics ({}): {} R, {} C, gesamt {}", "Parasites ({}) : {} R, {} C, total {}"],
    ["{} has no net {}", "{}에 넷 {}이(가) 없음", "{} にネット {} はない", "{} 中没有网络 {}", "{} no tiene la red {}", "{} hat kein Netz {}", "{} n'a pas de réseau {}"],
    ["{} (by position)", "{} (위치로)", "{}（位置から）", "{}（按位置）", "{} (por posición)", "{} (aus der Lage)", "{} (d'après la position)"],
    ["its point in the netlist", "넷리스트에 적힌 점", "ネットリストの座標", "网表中的坐标", "su punto en la netlist", "sein Punkt in der Netzliste", "son point dans la netlist"],
    ["the terminal of its layout device", "레이아웃 트랜지스터의 단자", "レイアウト上のデバイス端子", "版图器件的端子", "el terminal de su dispositivo en el layout", "der Anschluss seines Layout-Bauelements", "la borne de son dispositif dans le layout"],
    ["placed between its R neighbours by the R values", "이웃 R 값으로 사이에 놓음", "隣の R の値から間に配置", "按相邻 R 的阻值放在其间", "situado entre sus R vecinas según sus valores", "nach den Werten der Nachbar-R dazwischen gelegt", "placé entre ses R voisines selon leurs valeurs"],
    ["no position", "위치 없음", "位置なし", "无位置", "sin posición", "keine Lage", "sans position"],
    ["ends: {} ({}), {} ({})", "양 끝: {} ({}), {} ({})", "両端: {}（{}）、{}（{}）", "两端：{}（{}），{}（{}）", "extremos: {} ({}), {} ({})", "Enden: {} ({}), {} ({})", "extrémités : {} ({}), {} ({})"],
    ["no segment: {}", "세그먼트 없음: {}", "セグメントなし: {}", "无线段：{}", "sin segmento: {}", "kein Segment: {}", "pas de segment : {}"],
    ["A segment is the metal of one R between its two nodes (wire), the via of one R between layers, or the metal under a capacitor that no wire covers (patch). Each capacitor side sits on the segment under its point.", "세그먼트는 R 하나가 두 노드 사이에 차지하는 금속(배선), 층 사이 R의 비아(비아), 배선 R이 덮지 않는 C 아래 금속(패치)이에요. C의 각 쪽은 자기 좌표 밑의 세그먼트에 붙어요.", "セグメントは、1 つの R が 2 つのノード間に占める金属（配線）、層間の R のビア（ビア）、配線 R が覆わない容量の下の金属（パッチ）です。容量の各側は自分の座標の下のセグメントに付きます。", "线段是一个 R 在两个节点之间占据的金属（导线）、层间 R 的通孔（通孔），或没有导线 R 覆盖的电容下方的金属（块）。电容的每一侧都落在其坐标下的线段上。", "Un segmento es el metal de una R entre sus dos nodos (cable), la vía de una R entre capas (vía), o el metal bajo un condensador que ninguna R de cable cubre (parche). Cada lado de un condensador va en el segmento bajo su punto.", "Ein Segment ist das Metall einer R zwischen ihren zwei Knoten (Leitung), die Via einer R zwischen Lagen (Via) oder das Metall unter einer Kapazität, das keine Leitungs-R abdeckt (Fläche). Jede Seite einer Kapazität liegt auf dem Segment unter ihrem Punkt.", "Un segment est le métal d'une R entre ses deux nœuds (fil), le via d'une R entre couches (via), ou le métal sous une capacité qu'aucune R de fil ne couvre (zone). Chaque côté d'une capacité se place sur le segment sous son point."],
];

#[cfg(test)]
mod tests {
    use super::*;

    /// placeholder indices of a template, in order (implicit ones numbered as `fill` does)
    fn holes(t: &str) -> Vec<usize> {
        let (mut v, mut next) = (vec![], 0);
        let t = t.replace("{{", "").replace("}}", "");
        let mut rest = t.as_str();
        while let Some(a) = rest.find('{') {
            let b = rest[a..].find('}').expect("unclosed placeholder") + a;
            let pos = rest[a + 1..b].split(':').next().unwrap_or("");
            v.push(if pos.is_empty() {
                next += 1;
                next - 1
            } else {
                pos.parse().expect("bad placeholder index")
            });
            rest = &rest[b + 1..];
        }
        v
    }

    #[test]
    fn translations_fit_their_english() {
        let mut seen = std::collections::HashSet::new();
        for r in T {
            assert!(seen.insert(r[0]), "English text twice in the table: {:?}", r[0]);
            let en = holes(r[0]);
            for (k, t) in r.iter().enumerate().skip(1) {
                assert!(!t.contains(['\u{2014}', '\u{2015}', ';']), "dash or semicolon in {:?}", t);
                if t.is_empty() {
                    continue;
                }
                let h = holes(t);
                assert!(h.iter().all(|i| *i < en.len()), "{} {:?}: placeholder beyond the English ones", LANGS[k].1, t);
                let mut a = h.clone();
                a.sort();
                a.dedup();
                // a translation may drop an English-only plural suffix, nothing else
                assert!(a.len() + 1 >= en.len(), "{} {:?}: placeholders {:?} vs English {:?}", LANGS[k].1, t, h, en);
            }
        }
    }

    #[test]
    fn fill_matches_format() {
        let (a, b, c) = (3.14159f64, "ab", 42usize);
        assert_eq!(fill("{} | {:.1} s", &[&b, &a]), format!("{} | {:.1} s", b, a));
        assert_eq!(fill("{:<8}|{:>11}|{:>2}|{:.0}", &[&b, &a, &c, &a]), format!("{:<8}|{:>11}|{:>2}|{:.0}", b, a, c, a));
        assert_eq!(fill("{1} of {0}", &[&b, &c]), "42 of ab");
        assert_eq!(fill("{{x}} {}", &[&c]), "{x} 42");
        assert_eq!(fill("{} {}", &[&c]), "42 {}");
    }

    #[test]
    fn lookup_and_fallback() {
        set_lang(Lang::Ko);
        assert_eq!(tr("File"), "파일");
        assert_eq!(tr("not a UI text"), "not a UI text");
        assert_eq!(tr("fan-in"), "fan-in");
        assert!(made_from(&crate::trf!("Error: {}", "x"), "Error: {}"));
        assert!(made_from("Error: x", "Error: {}"));
        assert!(!made_from("Reading x", "Error: {}"));
        assert_eq!(crate::trf!("Netlist: {} cells of {} drawn ({}) | layout {} ms", 5, 10, "w", 3), "넷리스트: 셀 10개 중 5개 그림 (w) | 배치 3 ms");
        set_lang(Lang::En);
        assert_eq!(crate::trf!("{} cells", 2), "2 cells");
    }

    #[test]
    fn settings_lines() {
        assert_eq!(set_key("", "language", "ko"), "language=ko\n");
        let t = set_key("theme=dark\nlanguage=ko\n", "language", "fr");
        assert_eq!(t, "theme=dark\nlanguage=fr\n");
        assert_eq!(get_key(&t, "language"), Some("fr"));
        assert_eq!(get_key("x=1", "language"), None);
    }

    // on the real systems: each CJK language finds a font file and, in a collection, the named face
    #[test]
    #[cfg(any(target_os = "macos", windows))]
    fn system_cjk_fonts() {
        for l in [Lang::Ko, Lang::Ja, Lang::Zh] {
            let f = locate(l).unwrap_or_else(|| panic!("no {} font found", l.english()));
            let b = std::fs::read(&f.path).expect("font file");
            let names = face_names(&b);
            let i = f.index.unwrap_or_else(|| pick_face(&b, f.want)) as usize;
            let name = names.get(i).cloned().unwrap_or_default();
            println!("{}: {} face {} of {} = {}", l.english(), f.path.display(), i, names.len(), name);
            assert!(i < names.len(), "face index out of range");
            if !f.want.is_empty() {
                assert!(f.want.iter().any(|w| name.to_ascii_lowercase().contains(&w.to_ascii_lowercase())), "face {} is not one of {:?}", name, f.want);
            }
        }
    }
}
