use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
}

pub struct Config {
    pub devices: Vec<Entry>,
    pub path: PathBuf,
}

impl Config {
    pub fn dir() -> PathBuf {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
        base.join("AudioFlip")
    }

    /// Returns the config and whether the file already existed.
    pub fn load() -> (Self, bool) {
        let path = Self::dir().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => (Self { devices: parse(&text), path }, true),
            Err(_) => (Self { devices: Vec::new(), path }, false),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, serialize(&self.devices))
    }
}

// Minimal TOML subset: `[[device]]` tables with `id` / `name` basic strings.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut cur: Option<Entry> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[device]]" {
            out.extend(cur.take().filter(|e| !e.id.is_empty() || !e.name.is_empty()));
            cur = Some(Entry { id: String::new(), name: String::new() });
            continue;
        }
        let (Some(e), Some((k, v))) = (cur.as_mut(), line.split_once('=')) else {
            continue;
        };
        let Some(v) = unquote(v.trim()) else { continue };
        match k.trim() {
            "id" => e.id = v,
            "name" => e.name = v,
            _ => {}
        }
    }
    out.extend(cur.filter(|e| !e.id.is_empty() || !e.name.is_empty()));
    out
}

fn unquote(v: &str) -> Option<String> {
    let mut chars = v.strip_prefix('"')?.chars();
    let mut s = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(s),
            '\\' => match chars.next()? {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                c => s.push(c),
            },
            c => s.push(c),
        }
    }
}

fn quote(v: &str) -> String {
    let mut s = String::from('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

pub fn serialize(devices: &[Entry]) -> String {
    let mut s = String::from(
        "# AudioFlip 配置\n# 左键托盘图标按以下顺序轮换；匹配优先用 id，id 失效时回退到 name\n",
    );
    for e in devices {
        s.push_str(&format!("\n[[device]]\nname = {}\nid = {}\n", quote(&e.name), quote(&e.id)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let v = vec![
            Entry { id: "{0.0.0.00000000}.{abc}".into(), name: "耳机 (2- \"USB\" \\ Audio)".into() },
            Entry { id: "x".into(), name: "显示器".into() },
        ];
        assert_eq!(parse(&serialize(&v)), v);
    }

    #[test]
    fn tolerant() {
        let t = "garbage\n[[device]]\n  name = \"A\"  # c\nfoo = 1\n[[device]]\n[[device]]\nid=\"B\"";
        assert_eq!(
            parse(t),
            vec![
                Entry { id: "".into(), name: "A".into() },
                Entry { id: "B".into(), name: "".into() }
            ]
        );
    }
}
