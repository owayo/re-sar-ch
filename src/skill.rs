//! AI エージェント向けスキルのインストール。
//!
//! `skills/SKILL.md` をバイナリへ埋め込み (`include_str!`)、
//! エージェントごとの既定の置き場所へ書き出す。
//!
//! **ファイルを取りに行かない。** リリースバイナリを 1 本置くだけで
//! `resarch skill-install claude` が完結するようにするためで、
//! ネットワークもリポジトリのチェックアウトも要らない。

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 埋め込むスキル本文。
const SKILL_CONTENT: &str = include_str!("../skills/SKILL.md");

/// スキルのディレクトリ名。エージェント側はこの名前でスキルを引く。
const SKILL_NAME: &str = "resarch";

/// 対応しているエージェントと、その既定のスキル置き場。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code — `~/.claude/skills/resarch/SKILL.md`
    Claude,
    /// Codex CLI — `~/.codex/skills/resarch/SKILL.md`
    Codex,
}

impl Agent {
    /// 引数の文字列から選ぶ。
    ///
    /// 別名も受ける (`claude-code`)。未知の値は**候補を添えて**拒否する。
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(Error::Other(format!(
                "不明なインストール先です: \"{other}\" (指定できるのは {})",
                Self::ALL.join(" / ")
            ))),
        }
    }

    /// このエージェントのスキル置き場 (`<home>/.<agent>/skills/resarch`)。
    fn skill_dir(self, home: &Path) -> PathBuf {
        let base = match self {
            Self::Claude => home.join(".claude"),
            Self::Codex => home.join(".codex"),
        };
        base.join("skills").join(SKILL_NAME)
    }

    /// 報告に使う表示名。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex CLI",
        }
    }

    /// 指定できる値の一覧。
    ///
    /// **エラーメッセージはここから作る。** [`Agent::parse`] の分岐と
    /// 別に文字列を持つと、エージェントを足したときに案内だけ古くなる。
    pub const ALL: [&'static str; 2] = ["claude", "codex"];
}

/// スキルをインストールし、書き出した場所を返す。
///
/// 既にあれば**上書きする**。版が上がったときに古い本文が残ると、
/// エージェントが存在しないオプションを案内してしまうため。
pub fn install(agent: &str) -> Result<PathBuf> {
    let agent = Agent::parse(agent)?;
    let home = home_dir()?;
    install_into(agent, &home)
}

/// ホームディレクトリを決める。
///
/// `dirs` クレートを足さず環境変数で済ませている。スキルの置き場は
/// `HOME` 基準で決まる規約なので、それ以上の解決は要らない。
fn home_dir() -> Result<PathBuf> {
    // Windows では HOME が無いことがあるので USERPROFILE も見る
    for key in ["HOME", "USERPROFILE"] {
        if let Some(v) = std::env::var_os(key)
            && !v.is_empty()
        {
            return Ok(PathBuf::from(v));
        }
    }
    Err(Error::Other(
        "ホームディレクトリを判別できません (HOME も USERPROFILE も設定されていません)".into(),
    ))
}

/// 指定したホーム相当のディレクトリ配下へ書き出す (テストから直接呼ぶ)。
fn install_into(agent: Agent, home: &Path) -> Result<PathBuf> {
    let dir = agent.skill_dir(home);
    fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;

    let path = dir.join("SKILL.md");
    fs::write(&path, SKILL_CONTENT).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_names_and_aliases_are_accepted() {
        assert_eq!(Agent::parse("claude").unwrap(), Agent::Claude);
        assert_eq!(Agent::parse("claude-code").unwrap(), Agent::Claude);
        assert_eq!(Agent::parse("CLAUDE").unwrap(), Agent::Claude);
        assert_eq!(Agent::parse("codex").unwrap(), Agent::Codex);
    }

    /// 未知の指定は候補を添えて拒否する (黙って既定を選ばない)。
    #[test]
    fn an_unknown_agent_is_rejected_with_the_choices() {
        let err = Agent::parse("cursor").unwrap_err().to_string();
        assert!(err.contains("cursor"), "{err}");
        for name in Agent::ALL {
            assert!(err.contains(name), "候補 {name} が案内に無い: {err}");
        }
    }

    /// 案内する候補が**すべて実際に受け付けられる**こと。
    ///
    /// 分岐と一覧を別に持つので、エージェントを足したときに
    /// 片方だけ更新される事故を止める。
    #[test]
    fn every_advertised_agent_is_actually_accepted() {
        for name in Agent::ALL {
            Agent::parse(name).unwrap_or_else(|e| panic!("{name} を受け付けない: {e}"));
        }
    }

    #[test]
    fn the_skill_goes_under_the_agents_own_directory() {
        let home = Path::new("/nonexistent-home");
        assert_eq!(
            Agent::Claude.skill_dir(home),
            home.join(".claude").join("skills").join("resarch")
        );
        assert_eq!(
            Agent::Codex.skill_dir(home),
            home.join(".codex").join("skills").join("resarch")
        );
    }

    #[test]
    fn install_writes_the_embedded_skill() {
        let temp = tempfile::tempdir().unwrap();
        let path = install_into(Agent::Codex, temp.path()).unwrap();

        assert_eq!(
            path,
            temp.path()
                .join(".codex")
                .join("skills")
                .join("resarch")
                .join("SKILL.md")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL_CONTENT);
    }

    /// 版が上がったときに古い本文が残らないこと。
    #[test]
    fn install_overwrites_an_older_skill() {
        let temp = tempfile::tempdir().unwrap();
        let path = install_into(Agent::Claude, temp.path()).unwrap();
        fs::write(&path, "古い本文").unwrap();

        let again = install_into(Agent::Claude, temp.path()).unwrap();
        assert_eq!(again, path);
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL_CONTENT);
    }

    /// 埋め込んだ本文が**スキルとして成立している**こと。
    ///
    /// frontmatter が壊れるとエージェント側が読み込まないので、
    /// 名前・説明・許可ツールの 3 つを機械的に確かめる。
    #[test]
    fn the_embedded_skill_has_the_frontmatter_agents_require() {
        assert!(
            SKILL_CONTENT.starts_with("---\n"),
            "frontmatter で始まること"
        );
        let end = SKILL_CONTENT[4..]
            .find("\n---\n")
            .expect("frontmatter が閉じていること");
        let front = &SKILL_CONTENT[4..4 + end];

        assert!(
            front.contains(&format!("name: {SKILL_NAME}")),
            "ディレクトリ名と frontmatter の name を揃えること: {front}"
        );
        assert!(front.contains("description:"), "{front}");
        assert!(
            front.contains("allowed-tools: Bash(resarch:*)"),
            "実行を許可するのは resarch だけにする: {front}"
        );
    }

    /// 説明文に発動の手がかりが入っていること。
    ///
    /// `description` だけを見て発動を決めるエージェントがあるため、
    /// バイナリ名と代表的なサブコマンドは必ず含める。
    #[test]
    fn the_description_names_the_binary_and_its_entry_points() {
        let head = &SKILL_CONTENT[..SKILL_CONTENT.find("\n---\n").unwrap()];
        for word in ["resarch", "sa ", "sar", "sadf", "detect"] {
            assert!(head.contains(word), "description に {word:?} が無い");
        }
    }
}
