//! Application language and deferred text selection. Protocol values stay stable.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN", alias = "zh", alias = "zh-Hans")]
    Chinese,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::Chinese];

    pub fn text<'a>(self, english: &'a str, chinese: &'a str) -> &'a str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }

    pub fn label(self) -> &'static str {
        self.text("English", "中文")
    }

    pub fn tagline(self) -> &'static str {
        self.text("Space, made clear", "空间，清晰可见")
    }

    pub fn confirmation_word(self) -> &'static str {
        self.text("DELETE", "永久删除")
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Keep both variants for stateful banners/progress so a running view switches
/// immediately, even when no new service event has arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LocalizedText {
    english: String,
    chinese: String,
}

impl LocalizedText {
    pub fn new(english: impl Into<String>, chinese: impl Into<String>) -> Self {
        Self {
            english: english.into(),
            chinese: chinese.into(),
        }
    }

    pub fn get(&self, language: Language) -> &str {
        language.text(&self.english, &self.chinese)
    }
}

// OS/service diagnostics are technical text, not UI translation keys.
impl From<String> for LocalizedText {
    fn from(value: String) -> Self {
        Self::new(value.clone(), value)
    }
}

impl From<&str> for LocalizedText {
    fn from(value: &str) -> Self {
        Self::new(value, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_defaults_to_english_with_stable_persisted_codes() {
        assert_eq!(Language::default(), Language::English);
        assert_eq!(serde_json::to_string(&Language::English).unwrap(), "\"en\"");
        assert_eq!(
            serde_json::to_string(&Language::Chinese).unwrap(),
            "\"zh-CN\""
        );
        for code in ["zh-CN", "zh", "zh-Hans"] {
            assert_eq!(
                serde_json::from_value::<Language>(serde_json::json!(code)).unwrap(),
                Language::Chinese
            );
        }
    }

    #[test]
    fn stored_messages_select_the_current_language_without_mutation() {
        let message = LocalizedText::new("Scanning 12 files", "正在扫描 12 个文件");
        assert_eq!(message.get(Language::English), "Scanning 12 files");
        assert_eq!(message.get(Language::Chinese), "正在扫描 12 个文件");
        assert_eq!(message.get(Language::English), "Scanning 12 files");
    }
}
