// The Zed extension's one job (docs/tooling/04_extension.md §15): say
// where `decl-lsp` is. The setting `lsp.decl-lsp.binary.path` first, then
// `decl-lsp` on PATH (any of the three implementations — npm, PyPI,
// crates.io, or Homebrew — answers identically), else the prebuilt
// binary of the language's GitHub release for this platform, downloaded
// into the extension's work directory and kept current with the release.
// Settings under `lsp.decl-lsp.settings` are forwarded as the server's
// workspace configuration (the same keys VS Code's `decl.*` use).
use zed_extension_api::{self as zed, settings::LspSettings, LanguageServerId, Result};

const RELEASE_REPO: &str = "luuvish/decl-lang";

struct DeclExtension {
    cached_binary: Option<String>,
}

impl DeclExtension {
    fn binary(&mut self, id: &LanguageServerId, worktree: &zed::Worktree) -> Result<String> {
        if let Some(path) = LspSettings::for_worktree("decl-lsp", worktree)
            .ok()
            .and_then(|s| s.binary)
            .and_then(|b| b.path)
        {
            return Ok(path);
        }
        if let Some(path) = worktree.which("decl-lsp") {
            return Ok(path);
        }
        if let Some(path) = &self.cached_binary {
            if std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false) {
                return Ok(path.clone());
            }
        }
        zed::set_language_server_installation_status(id, &zed::LanguageServerInstallationStatus::CheckingForUpdate);
        let release = zed::latest_github_release(RELEASE_REPO, zed::GithubReleaseOptions { require_assets: true, pre_release: false })?;
        let (os, arch) = zed::current_platform();
        let asset_name = format!(
            "decl-lsp-{}-{}{}",
            match os { zed::Os::Mac => "macos", zed::Os::Linux => "linux", zed::Os::Windows => "windows" },
            match arch { zed::Architecture::Aarch64 => "arm64", zed::Architecture::X8664 => "x86_64", zed::Architecture::X86 => "x86" },
            if os == zed::Os::Windows { ".exe" } else { "" },
        );
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| format!("the release {} has no asset {asset_name}", release.version))?;
        let dir = format!("decl-lsp-{}", release.version);
        let path = format!("{dir}/{asset_name}");
        if !std::fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false) {
            zed::set_language_server_installation_status(id, &zed::LanguageServerInstallationStatus::Downloading);
            std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
            zed::download_file(&asset.download_url, &path, zed::DownloadedFileType::Uncompressed)?;
            zed::make_file_executable(&path)?;
            // older releases are not needed once the current one is in place
            if let Ok(entries) = std::fs::read_dir(".") {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with("decl-lsp-") && name != dir {
                        let _ = std::fs::remove_dir_all(e.path());
                    }
                }
            }
        }
        self.cached_binary = Some(path.clone());
        Ok(path)
    }
}

impl zed::Extension for DeclExtension {
    fn new() -> Self {
        Self { cached_binary: None }
    }

    fn language_server_command(&mut self, id: &LanguageServerId, worktree: &zed::Worktree) -> Result<zed::Command> {
        let command = self.binary(id, worktree)?;
        let args = LspSettings::for_worktree("decl-lsp", worktree)
            .ok()
            .and_then(|s| s.binary)
            .and_then(|b| b.arguments)
            .unwrap_or_default();
        Ok(zed::Command { command, args, env: Default::default() })
    }

    fn language_server_workspace_configuration(&mut self, _id: &LanguageServerId, worktree: &zed::Worktree) -> Result<Option<zed::serde_json::Value>> {
        // `lsp.decl-lsp.settings` → the server's `decl` configuration (03_lsp.md §14)
        let settings = LspSettings::for_worktree("decl-lsp", worktree).ok().and_then(|s| s.settings).unwrap_or_default();
        Ok(Some(zed::serde_json::json!({ "decl": settings })))
    }

    fn language_server_initialization_options(&mut self, _id: &LanguageServerId, worktree: &zed::Worktree) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree("decl-lsp", worktree).ok().and_then(|s| s.initialization_options))
    }
}

zed::register_extension!(DeclExtension);
