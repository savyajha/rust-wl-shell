//! A keyboard layout's short name, as GNOME shows it: xkeyboard-config's
//! `<shortDescription>` for the layout's description ("German" is "de"). niri names
//! layouts by that description. The rules file's path is set at build time
//! (`XKB_EVDEV_XML`); without it, or for a layout it does not list, the short name is the
//! description's first two letters.

use std::fs;
use std::sync::OnceLock;

/// The short name of the layout described as `description`, in lower case.
pub fn short_name(description: &str) -> String {
    static NAMES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let path = option_env!("XKB_EVDEV_XML").unwrap_or_default();
        parse(&fs::read_to_string(path).unwrap_or_default())
    });
    let listed = names.iter().find(|(listed, _)| listed == description);
    listed.map_or_else(
        || {
            description
                .chars()
                .take(2)
                .collect::<String>()
                .to_lowercase()
        },
        |(_, short)| short.clone(),
    )
}

/// Each layout's and variant's description and short name, from the rules file's text. Its
/// layout list has one element per line and no entities; a variant without a short name
/// has its layout's.
fn parse(text: &str) -> Vec<(String, String)> {
    let element = |line: &str, name: &str| {
        let line = line.trim();
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        Some(line.strip_prefix(&open)?.strip_suffix(&close)?.to_string())
    };
    let (mut names, mut layout, mut variant) = (Vec::new(), String::new(), None);
    // The models before it and the options after it are described too.
    let start = text.find("<layoutList>").unwrap_or(text.len());
    let end = text.find("</layoutList>").unwrap_or(text.len()).max(start);
    for line in text[start..end].lines() {
        match line.trim() {
            "<layout>" => layout.clear(),
            "<variant>" | "</variant>" => variant = None,
            _ => {}
        }
        if let Some(short) = element(line, "shortDescription") {
            // The layout's own comes first, before its variants'.
            if layout.is_empty() {
                layout = short;
            } else {
                variant = Some(short);
            }
        } else if let Some(description) = element(line, "description") {
            let short = variant.clone().unwrap_or_else(|| layout.clone());
            names.push((description, short));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = r#"
    <modelList><model><configItem>
        <description>Generic 105-key PC</description>
    </configItem></model></modelList>
    <layoutList>
    <layout>
      <configItem>
        <name>us</name>
        <shortDescription>en</shortDescription>
        <description>English (US)</description>
      </configItem>
      <variantList>
        <variant>
          <configItem>
            <name>chr</name>
            <shortDescription>chr</shortDescription>
            <description>Cherokee</description>
          </configItem>
        </variant>
        <variant>
          <configItem>
            <name>dvorak</name>
            <description>English (Dvorak)</description>
          </configItem>
        </variant>
      </variantList>
    </layout>
    <layout>
      <configItem>
        <name>de</name>
        <shortDescription>de</shortDescription>
        <description>German</description>
      </configItem>
    </layout>
    </layoutList>
    "#;

    #[test]
    fn layouts_and_variants_have_their_short_names() {
        let names = parse(RULES);
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            names,
            [
                pair("English (US)", "en"),
                pair("Cherokee", "chr"),
                pair("English (Dvorak)", "en"),
                pair("German", "de"),
            ]
        );
    }

    #[test]
    fn the_installed_rules_are_read() {
        // Built by Nix, XKB_EVDEV_XML points at xkeyboard-config's rules.
        if option_env!("XKB_EVDEV_XML").is_some() {
            assert_eq!(short_name("German"), "de");
            assert_eq!(short_name("English (US)"), "en");
        }
        assert_eq!(short_name("Not A Layout"), "no");
    }
}
