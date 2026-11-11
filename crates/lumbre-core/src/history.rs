//! Conversations on disk: one JSON file each, in the user's data directory. Nothing leaves it.
//!
//! Search is a linear, case-insensitive scan. That is fine for thousands of conversations; an
//! index is future work if it ever is not.

use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::Local;
use lumbre_api::{Conversation, ConversationSummary, Role, StoredMessage};

#[derive(Clone, Debug)]
pub struct History {
    dir: PathBuf,
}

fn now() -> String {
    Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn not_found(id: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no conversation `{id}`"))
}

/// The first line of the first message, shortened.
fn title_from(text: &str) -> String {
    let line = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
    let mut title: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        title.push('…');
    }
    title
}

fn snippet(content: &str, query: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let at = lower.find(query)?;
    // Byte offsets of `lower` can differ from `content`; work on characters of the lowercase text.
    let before = lower[..at].chars().count();
    let chars: Vec<char> = content.chars().collect();
    let start = before.saturating_sub(40);
    let end = (before + query.chars().count() + 60).min(chars.len());
    let mut text: String = chars.get(start..end)?.iter().collect::<String>().replace('\n', " ");
    if start > 0 {
        text.insert(0, '…');
    }
    if end < chars.len() {
        text.push('…');
    }
    Some(text)
}

impl History {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(not_found(id));
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    fn write(&self, conversation: &Conversation) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(&conversation.id)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(conversation)?)?;
        // Replace atomically so a crash never leaves half a conversation.
        fs::rename(temporary, path)
    }

    pub fn get(&self, id: &str) -> io::Result<Conversation> {
        let bytes = fs::read(self.path(id)?).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => not_found(id),
            _ => error,
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Appends one exchange. `id` of `None` or `"new"` starts a conversation. Returns its id.
    pub fn append(&self, id: Option<&str>, user: &str, assistant: &str, via: &str) -> io::Result<String> {
        let at = now();
        let mut conversation = match id.filter(|id| *id != "new") {
            Some(id) => self.get(id)?,
            None => {
                let stamp = Local::now().format("%Y%m%d-%H%M%S");
                // Two conversations started in the same second get different ids.
                let id = (0u32..)
                    .map(|n| if n == 0 { stamp.to_string() } else { format!("{stamp}-{n}") })
                    .find(|id| !self.dir.join(format!("{id}.json")).exists())
                    .unwrap_or_default();
                Conversation {
                    id,
                    title: title_from(user),
                    created: at.clone(),
                    updated: at.clone(),
                    messages: Vec::new(),
                }
            }
        };
        conversation.messages.push(StoredMessage {
            role: Role::User,
            content: user.to_owned(),
            at: at.clone(),
            via: None,
        });
        conversation.messages.push(StoredMessage {
            role: Role::Assistant,
            content: assistant.to_owned(),
            at: at.clone(),
            via: Some(via.to_owned()),
        });
        conversation.updated = at;
        self.write(&conversation)?;
        Ok(conversation.id)
    }

    /// Newest first. With a query, only conversations whose title or messages contain it.
    pub fn list(&self, query: Option<&str>) -> Vec<ConversationSummary> {
        let query = query.map(|query| query.trim().to_lowercase()).filter(|query| !query.is_empty());
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(conversation) =
                fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Conversation>(&bytes).ok())
            else {
                continue;
            };
            let hit = match &query {
                None => None,
                Some(query) => {
                    let in_messages =
                        conversation.messages.iter().find_map(|message| snippet(&message.content, query));
                    match in_messages.or_else(|| snippet(&conversation.title, query)) {
                        Some(hit) => Some(hit),
                        None => continue,
                    }
                }
            };
            found.push(ConversationSummary {
                id: conversation.id,
                title: conversation.title,
                updated: conversation.updated,
                messages: conversation.messages.len(),
                snippet: hit,
            });
        }
        found.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        found
    }

    pub fn delete(&self, id: &str) -> io::Result<()> {
        fs::remove_file(self.path(id)?).map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => not_found(id),
            _ => error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(name: &str) -> History {
        let dir = std::env::temp_dir().join(format!("lumbre-history-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        History::new(dir)
    }

    #[test]
    fn append_get_list_search_delete() {
        let history = history("crud");
        assert!(history.list(None).is_empty());

        let first = history.append(None, "¿Qué hora es?", "Son las 09:05.", "tool:clock.now").unwrap();
        let second = history
            .append(
                Some("new"),
                "Háblame de la Alhambra de Granada",
                "Es un conjunto palaciego…",
                "qwen3-4b:Q4_K_M",
            )
            .unwrap();
        assert_ne!(first, second);
        history.append(Some(&first), "¿Y la fecha?", "Hoy es miércoles.", "tool:clock.now").unwrap();

        let conversation = history.get(&first).unwrap();
        assert_eq!(conversation.title, "¿Qué hora es?");
        assert_eq!(conversation.messages.len(), 4);
        assert_eq!(conversation.messages[1].via.as_deref(), Some("tool:clock.now"));

        assert_eq!(history.list(None).len(), 2);
        let hits = history.list(Some("ALHAMBRA"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, second);
        assert_eq!(hits[0].snippet.as_deref(), Some("Háblame de la Alhambra de Granada"));
        assert!(history.list(Some("zaragoza")).is_empty());

        history.delete(&second).unwrap();
        assert_eq!(history.delete(&second).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(history.list(None).len(), 1);
        fs::remove_dir_all(&history.dir).unwrap();
    }

    #[test]
    fn ids_cannot_escape_the_directory() {
        let history = history("ids");
        for id in ["../secret", "a/b", "", "x.json"] {
            assert_eq!(history.get(id).unwrap_err().kind(), io::ErrorKind::NotFound, "{id}");
            assert!(history.append(Some(id), "a", "b", "c").is_err(), "{id}");
        }
    }

    #[test]
    fn snippets_and_titles_are_trimmed_around_the_match() {
        let long = format!("{}aguja{}", "a ".repeat(100), " b".repeat(100));
        let text = snippet(&long, "aguja").unwrap();
        assert!(text.starts_with('…') && text.ends_with('…') && text.contains("aguja"));
        assert!(text.chars().count() < 120);
        assert_eq!(title_from(&"x".repeat(80)).chars().count(), 61);
        assert_eq!(title_from("\n\n  Hola  \nmundo"), "Hola");
    }
}
