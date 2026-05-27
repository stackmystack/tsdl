use std::{
    env::consts::DLL_EXTENSION,
    fmt,
    fs::Metadata,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{fs, process::Command};
use tracing::{debug, warn};

use crate::args::TreeSitter;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    actors::ProgressAddr,
    build::{BuildContext, BuildSpec, OutputConfig},
    cache::{self, CacheDecision, CacheKey, Entry, GrammarHash, Update},
    error::{self, TsdlError},
    git,
    sh::{Exec, Script},
    shutdown,
    walk::collect_grammar_paths,
    TsdlResult,
};

pub const WASM_EXTENSION: &str = "wasm";

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LanguageName(Arc<str>);

impl LanguageName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_arc(&self) -> Arc<str> {
        self.0.clone()
    }
}

impl fmt::Display for LanguageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for LanguageName {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for LanguageName {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<Arc<str>> for LanguageName {
    fn from(value: Arc<str>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrammarName(Arc<str>);

impl GrammarName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_arc(&self) -> Arc<str> {
        self.0.clone()
    }
}

impl fmt::Display for GrammarName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for GrammarName {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for GrammarName {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<Arc<str>> for GrammarName {
    fn from(value: Arc<str>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ArtifactKind {
    Native,
    Wasm,
}

impl ArtifactKind {
    #[must_use]
    fn extension(self) -> &'static str {
        match self {
            Self::Native => DLL_EXTENSION,
            Self::Wasm => WASM_EXTENSION,
        }
    }

    #[must_use]
    const fn is_wasm(self) -> bool {
        matches!(self, Self::Wasm)
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum Ref {
    Stable(git::Ref),
    Moving(git::Ref),
}

impl Ref {
    /// The default source ref used for unpinned parser builds.
    #[must_use]
    pub fn head() -> Self {
        Self::Moving(git::Ref::head())
    }

    /// Create a parser source ref from user input, preserving parser-version
    /// normalization while classifying refs for cache semantics.
    pub fn parse(value: &str) -> Result<Self, git::RefError> {
        let normalized = normalize_source_ref(value);
        let git_ref = git::Ref::new(normalized)?;
        if is_stable_source_ref(value, &git_ref) {
            Ok(Self::Stable(git_ref))
        } else {
            Ok(Self::Moving(git_ref))
        }
    }

    #[must_use]
    pub const fn is_moving(&self) -> bool {
        matches!(self, Self::Moving(_))
    }

    #[must_use]
    pub const fn is_stable(&self) -> bool {
        matches!(self, Self::Stable(_))
    }

    #[must_use]
    pub fn requested(&self) -> &git::Ref {
        match self {
            Self::Stable(git_ref) | Self::Moving(git_ref) => git_ref,
        }
    }
}

impl Serialize for Ref {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.requested().as_str())
    }
}

impl<'de> Deserialize<'de> for Ref {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.requested())
    }
}

fn is_dotted_numeric_version(value: &str) -> bool {
    !value.is_empty()
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.parse::<u32>().is_ok())
}

fn normalize_source_ref(value: &str) -> String {
    if git::Sha::is_full_sha(value) || value.starts_with('v') {
        value.to_string()
    } else if is_dotted_numeric_version(value) {
        format!("v{value}")
    } else {
        value.to_string()
    }
}

fn is_v_dotted_numeric_version(value: &str) -> bool {
    value
        .strip_prefix('v')
        .is_some_and(is_dotted_numeric_version)
}

fn is_stable_source_ref(input: &str, git_ref: &git::Ref) -> bool {
    git_ref.is_exact_sha()
        || is_dotted_numeric_version(input)
        || is_v_dotted_numeric_version(input)
        || git_ref.as_str().starts_with("refs/tags/")
}

/// A grammar ready to be built, combining definition and cache state
#[derive(Clone, Debug)]
pub struct GrammarBuild {
    pub context: BuildContext,
    pub cache_decision: CacheDecision,
    pub dir: Arc<PathBuf>,
    pub hash: GrammarHash,
    pub language: LanguageName, // Required for error reporting and cache keys; set from parent LanguageBuild
    pub name: GrammarName,
    pub output: OutputConfig,
    pub progress: ProgressAddr, // Use language's handle
    pub source: cache::Source,
    pub spec: Arc<BuildSpec>,
    pub ts_cli: Arc<PathBuf>,
}

impl GrammarBuild {
    /// Build this grammar, returning a cache update if it was built.
    /// Uses the language's progress handle for progress reporting.
    pub async fn build(&self) -> TsdlResult<Option<Update>> {
        shutdown::test_delay().await;
        shutdown::check()?;
        debug!(
            "[grammar:build] start: lang={} grammar={}",
            self.language, self.name
        );

        self.progress.step("checking cache");
        let key = CacheKey::new(&self.language, &self.name);

        // Cache decisions are computed centrally by the cache actor, including
        // verification that all required managed artifacts exist.
        debug!("[grammar:cache] {key}: {}", self.cache_decision);

        if self.cache_decision.is_hit() {
            self.progress.set_outcome_cached().await;
            // Install the binary from the build directory
            if let Err(e) = self.install().await {
                self.progress.err("install failed").await;
                return Err(e);
            }

            self.progress.cached("done").await;
            return Ok(None);
        }

        self.progress.set_outcome_built().await;
        self.progress.msg(self.cache_decision.short_message());

        // Use the grammar directory path provided
        if !self.dir.exists() {
            let err = TsdlError::message(format!(
                "Grammar directory not found: {}",
                self.dir.display()
            ));
            self.progress.err("missing grammar directory").await;
            return Err(err);
        }

        // Build the grammar
        if let Err(e) = self.build_grammar().await {
            if shutdown::current().is_some_and(|s| s.is_cancelled()) {
                self.progress.cancel().await;
            } else {
                self.progress.err("build failed").await;
            }
            return Err(e);
        }

        // Return cache update for this grammar
        let update = Update {
            name: key,
            entry: Entry {
                hash: self.hash.clone(),
                spec: self.spec.clone(),
                source: self.source.clone(),
            },
        };

        self.progress.fin("done").await;

        Ok(Some(update))
    }

    fn builtin_build_command(&self, kind: ArtifactKind, output_path: &Path) -> Command {
        let mut cmd = Command::new(self.ts_cli.as_os_str());
        cmd.arg("build");

        if kind.is_wasm() {
            cmd.arg("--wasm");
        }

        cmd.arg("--output").arg(output_path);
        cmd
    }

    async fn build_grammar(&self) -> TsdlResult<()> {
        shutdown::test_delay().await;
        shutdown::check()?;

        // Generate parser if no custom build script
        self.progress.step("generating");
        if self.spec.build_script.is_none() {
            self.generate().await?;
        } else {
            warn!("Custom build scripts not supported for generate step (TypeScript limitation)");
        }

        // Build native and/or wasm targets
        self.progress.step("building");
        self.build_targets().await?;

        // Install built parsers
        self.progress.step("installing");
        self.install().await?;

        Ok(())
    }

    async fn build_target(&self, kind: ArtifactKind) -> TsdlResult<PathBuf> {
        shutdown::test_delay().await;
        shutdown::check()?;
        let ext = kind.extension();
        debug!(
            "[grammar:build_target] lang={} grammar={} ext={ext}",
            self.language, self.name
        );

        if let Some(script) = &self.spec.build_script {
            return self.build_custom_target(kind, script).await;
        }

        self.build_builtin_target(kind).await
    }

    async fn build_builtin_target(&self, kind: ArtifactKind) -> TsdlResult<PathBuf> {
        let artifact = self.artifact_path(kind)?;
        ensure_parent_dir(&artifact).await?;

        let mut cmd = self.builtin_build_command(kind, &artifact);
        cmd.current_dir(self.dir.as_ref())
            .exec()
            .await
            .map_err(|err| self.build_step_error(err))?;

        verify_artifact(&artifact).await?;
        Ok(artifact)
    }

    async fn build_custom_target(&self, kind: ArtifactKind, script: &str) -> TsdlResult<PathBuf> {
        let mut cmd = Command::from_str(script);
        cmd.current_dir(self.dir.as_ref())
            .exec()
            .await
            .map_err(|err| self.build_step_error(err))?;

        let discovered = self.brute_force_discover(kind).await?;
        let artifact = self.artifact_path(kind)?;
        self.stage_artifact(&discovered, &artifact).await?;
        verify_artifact(&artifact).await?;
        Ok(artifact)
    }

    fn build_step_error(&self, err: TsdlError) -> TsdlError {
        error::TsdlError::Step(error::Step::new(
            self.language.as_arc(),
            error::ParserOp::Build {
                dir: self.dir.to_path_buf(),
            },
            err,
        ))
    }

    async fn build_targets(&self) -> TsdlResult<()> {
        if self.spec.target.native() {
            self.build_target(ArtifactKind::Native).await?;
        }

        if self.spec.target.wasm() {
            self.build_target(ArtifactKind::Wasm).await?;
        }

        Ok(())
    }

    async fn create_hardlink(&self, src: &Path, dst: &Path) -> TsdlResult<()> {
        fs::hard_link(src, dst).await.map_err(|e| {
            TsdlError::context(
                format!(
                    "Could not hardlink {} to {}. build-dir and out-dir must be on the same filesystem",
                    src.display(),
                    dst.display()
                ),
                e,
            )
        })
    }

    async fn brute_force_discover(&self, kind: ArtifactKind) -> TsdlResult<PathBuf> {
        let ext = kind.extension();
        let expected_name = self.parser_name_and_ext(kind);
        let mut files = fs::read_dir(self.dir.as_ref()).await.map_err(|e| {
            TsdlError::context(
                format!("Failed to read directory {}", self.dir.display()),
                e,
            )
        })?;

        let mut exact_match = None;
        let mut candidates = Vec::new();

        loop {
            let Some(entry) = files.next_entry().await.map_err(|e| {
                TsdlError::context(
                    format!("Failed to read directory entry in {}", self.dir.display()),
                    e,
                )
            })?
            else {
                break;
            };

            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|e| {
                TsdlError::context(
                    format!("Failed to read file type for {}", path.display()),
                    e,
                )
            })?;

            if !file_type.is_file() {
                continue;
            }

            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            if name == expected_name {
                exact_match = Some(path);
                break;
            }

            if Path::new(&file_name).extension().and_then(|e| e.to_str()) == Some(ext) {
                candidates.push(path);
            }
        }

        match (exact_match, candidates.len()) {
            (Some(path), _) => Ok(path),
            (None, 0) => Err(self.missing_parser_error(ext)),
            (None, 1) => Ok(candidates.into_iter().next().unwrap()),
            (None, _) => Err(self.multiple_parsers_error(ext, &candidates)),
        }
    }

    async fn generate(&self) -> TsdlResult<()> {
        Command::new(self.ts_cli.as_os_str())
            .current_dir(self.dir.as_path())
            .arg("generate")
            .exec()
            .await
            .map(|_| ())
            .map_err(|err| {
                error::TsdlError::Step(error::Step::new(
                    self.language.as_arc(),
                    error::ParserOp::Generate {
                        dir: self.dir.to_path_buf(),
                    },
                    err,
                ))
            })
    }

    async fn install(&self) -> TsdlResult<()> {
        // Find and install parser binary for each extension
        if self.spec.target.native() {
            self.install_binary(ArtifactKind::Native).await?;
        }

        if self.spec.target.wasm() {
            self.install_binary(ArtifactKind::Wasm).await?;
        }

        Ok(())
    }

    async fn install_binary(&self, kind: ArtifactKind) -> TsdlResult<()> {
        let src = self.artifact_path(kind)?;
        let dst = self.output.out_dir.join(self.parser_name_and_ext(kind));
        let src_metadata = fs::metadata(&src)
            .await
            .map_err(|e| TsdlError::context(format!("Reading {}", src.display()), e))?;

        let dst_link_metadata = match fs::symlink_metadata(&dst).await {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.create_hardlink(&src, &dst).await?;
                return Ok(());
            }
            Err(err) => {
                return Err(TsdlError::context(
                    format!("Reading {}", dst.display()),
                    err,
                ));
            }
        };

        self.install_over_existing(&src, &src_metadata, &dst, &dst_link_metadata)
            .await
    }

    async fn install_over_existing(
        &self,
        src: &Path,
        src_metadata: &Metadata,
        dst: &Path,
        dst_link_metadata: &Metadata,
    ) -> TsdlResult<()> {
        let dst_file_type = dst_link_metadata.file_type();

        if dst_file_type.is_dir() {
            return Err(TsdlError::message(format!(
                "Output path is a directory and cannot be replaced: {}",
                dst.display()
            )));
        }

        if dst_file_type.is_symlink() {
            if !self.context.overwrite_output {
                return Err(TsdlError::message(format!(
                    "Output path is a symlink and will not be replaced without --force: {}",
                    dst.display()
                )));
            }

            self.replace_with_hardlink(src, dst).await?;
            self.progress.msg("Reinstalled");
            return Ok(());
        }

        if !dst_file_type.is_file() {
            return Err(TsdlError::message(format!(
                "Output path is not a regular file and cannot be replaced: {}",
                dst.display()
            )));
        }

        let dst_metadata = fs::metadata(dst)
            .await
            .map_err(|e| TsdlError::context(format!("Reading {}", dst.display()), e))?;

        if same_file_identity(src_metadata, &dst_metadata) {
            return Ok(());
        }

        let same_contents =
            same_regular_file_contents(src, src_metadata, dst, &dst_metadata).await?;

        if !same_contents && !self.context.overwrite_output {
            return Err(TsdlError::message(format!(
                "Output already exists and differs from the built parser: {}. Use --force to replace it.",
                dst.display()
            )));
        }

        self.replace_with_hardlink(src, dst).await?;
        self.progress.msg("reinstalled");
        Ok(())
    }

    async fn stage_artifact(&self, src: &Path, dst: &Path) -> TsdlResult<()> {
        ensure_parent_dir(dst).await?;
        let src_metadata = fs::metadata(src)
            .await
            .map_err(|e| TsdlError::context(format!("Reading {}", src.display()), e))?;

        if !src_metadata.is_file() {
            return Err(TsdlError::message(format!(
                "Discovered parser artifact is not a regular file: {}",
                src.display()
            )));
        }

        match fs::metadata(dst).await {
            Ok(dst_metadata) if same_file_identity(&src_metadata, &dst_metadata) => return Ok(()),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(TsdlError::context(
                    format!("Reading {}", dst.display()),
                    err,
                ));
            }
        }

        self.replace_with_hardlink(src, dst).await
    }

    pub fn required_artifacts_for(
        build_dir: &Path,
        ts_cli: &Path,
        spec: &BuildSpec,
        grammar_name: &GrammarName,
    ) -> TsdlResult<Vec<PathBuf>> {
        let mut artifacts = Vec::new();

        if spec.target.native() {
            artifacts.push(artifact_path_for(
                build_dir,
                ts_cli,
                spec,
                grammar_name,
                ArtifactKind::Native,
            )?);
        }

        if spec.target.wasm() {
            artifacts.push(artifact_path_for(
                build_dir,
                ts_cli,
                spec,
                grammar_name,
                ArtifactKind::Wasm,
            )?);
        }

        Ok(artifacts)
    }

    fn artifact_path(&self, kind: ArtifactKind) -> TsdlResult<PathBuf> {
        artifact_path_for(
            &self.output.build_dir,
            &self.ts_cli,
            &self.spec,
            &self.name,
            kind,
        )
    }

    async fn replace_with_hardlink(&self, src: &Path, dst: &Path) -> TsdlResult<()> {
        let tmp = temp_install_path(dst)?;
        self.create_hardlink(src, &tmp).await?;

        if let Err(err) = fs::rename(&tmp, dst).await {
            let _ = fs::remove_file(&tmp).await;
            return Err(TsdlError::context(
                format!("Installing {} to {}", src.display(), dst.display()),
                err,
            ));
        }

        Ok(())
    }
    fn missing_parser_error(&self, ext: &str) -> TsdlError {
        error::TsdlError::Step(error::Step::new(
            self.language.as_arc(),
            error::ParserOp::Copy {
                src: self.output.out_dir.to_path_buf(),
                dst: self.output.build_dir.to_path_buf(),
            },
            TsdlError::message(format!("Couldn't find any {ext} file")),
        ))
    }

    fn multiple_parsers_error(&self, ext: &str, candidates: &[PathBuf]) -> TsdlError {
        error::TsdlError::Step(error::Step::new(
            self.language.as_arc(),
            error::ParserOp::Copy {
                src: self.output.out_dir.to_path_buf(),
                dst: self.output.build_dir.to_path_buf(),
            },
            TsdlError::message(format!("Found multiple {ext} files: {candidates:?}")),
        ))
    }

    fn parser_name_and_ext(&self, kind: ArtifactKind) -> String {
        parser_name_and_ext(&self.spec.prefix, &self.name, kind)
    }
}

#[derive(Clone, Debug)]
pub struct LanguageBuild {
    pub context: BuildContext,
    pub spec: Arc<BuildSpec>,
    pub name: LanguageName,
    pub output: OutputConfig,
}

impl LanguageBuild {
    #[must_use]
    pub fn new(
        context: BuildContext,
        spec: Arc<BuildSpec>,
        name: LanguageName,
        output: OutputConfig,
    ) -> Self {
        Self {
            context,
            spec,
            name,
            output,
        }
    }

    pub async fn discover_grammars(&self) -> TsdlResult<Vec<(GrammarName, PathBuf, GrammarHash)>> {
        let file_results = collect_grammar_paths(self.output.build_dir.clone()).await?;
        let mut grammars = Vec::new();

        for (grammar_path, hash) in file_results {
            let grammar_dir = grammar_path.parent().ok_or_else(|| {
                TsdlError::Message(format!(
                    "Could not get parent directory for {}",
                    grammar_path.display()
                ))
            })?;
            let grammar_name = extract_grammar_name(grammar_dir)?;
            grammars.push((grammar_name, grammar_dir.to_path_buf(), hash));
        }

        Ok(grammars)
    }

    #[must_use]
    pub fn with_tree_sitter(mut self, tree_sitter: TreeSitter) -> Self {
        let mut spec = self.spec.as_ref().clone();
        spec.tree_sitter = tree_sitter;
        self.spec = Arc::new(spec);
        self
    }

    pub async fn checkout(&self) -> TsdlResult<git::Checkout> {
        git::checkout(
            self.spec.repo.as_str(),
            self.spec.git_ref.requested(),
            &self.output.build_dir,
        )
        .await
        .map_err(|err| {
            error::TsdlError::Step(error::Step::new(
                self.name.as_arc(),
                error::ParserOp::Clone {
                    dir: self.output.build_dir.to_path_buf(),
                },
                err,
            ))
        })
    }

    pub async fn is_checkout_usable(&self) -> bool {
        git::is_checkout_usable(self.spec.repo.as_str(), &self.output.build_dir).await
    }
}

fn parser_name_and_ext(prefix: &str, grammar_name: &GrammarName, kind: ArtifactKind) -> String {
    format!("{prefix}{grammar_name}.{}", kind.extension())
}

fn artifact_path_for(
    build_dir: &Path,
    ts_cli: &Path,
    spec: &BuildSpec,
    grammar_name: &GrammarName,
    kind: ArtifactKind,
) -> TsdlResult<PathBuf> {
    Ok(build_dir
        .join(artifact_dir_name_from_tree_sitter_cli(ts_cli)?)
        .join(parser_name_and_ext(&spec.prefix, grammar_name, kind)))
}

fn artifact_dir_name_from_tree_sitter_cli(ts_cli: &Path) -> TsdlResult<String> {
    let file_name = ts_cli
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            TsdlError::message(format!(
                "Could not derive artifact id from tree-sitter CLI path {}",
                ts_cli.display()
            ))
        })?;
    let id = file_name.strip_prefix("tree-sitter-").unwrap_or(file_name);
    Ok(format!("tsdl-{}", sanitize_path_component(id)))
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();

    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

async fn ensure_parent_dir(path: &Path) -> TsdlResult<()> {
    let parent = path.parent().ok_or_else(|| {
        TsdlError::message(format!(
            "Could not determine parent directory for {}",
            path.display()
        ))
    })?;

    fs::create_dir_all(parent)
        .await
        .map_err(|e| TsdlError::context(format!("Creating {}", parent.display()), e))
}

async fn verify_artifact(path: &Path) -> TsdlResult<()> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|e| TsdlError::context(format!("Reading built artifact {}", path.display()), e))?;

    if metadata.is_file() {
        Ok(())
    } else {
        Err(TsdlError::message(format!(
            "Built artifact is not a regular file: {}",
            path.display()
        )))
    }
}

fn same_file_identity(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

async fn same_regular_file_contents(
    src: &Path,
    src_metadata: &Metadata,
    dst: &Path,
    dst_metadata: &Metadata,
) -> TsdlResult<bool> {
    if src_metadata.len() != dst_metadata.len() {
        return Ok(false);
    }

    let src_hash = cache::hash_file(src).await?;
    let dst_hash = cache::hash_file(dst).await?;
    Ok(src_hash == dst_hash)
}

fn temp_install_path(dst: &Path) -> TsdlResult<PathBuf> {
    let file_name = dst.file_name().ok_or_else(|| {
        TsdlError::message(format!(
            "Could not create temporary install path for {}",
            dst.display()
        ))
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| {
            TsdlError::message(format!(
                "System clock is before UNIX epoch while creating temporary install path: {e}"
            ))
        })?
        .as_nanos();
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tsdl-{}-{nanos}", process::id()));

    Ok(dst.with_file_name(tmp_name))
}

fn extract_dir_name(dir: &Path) -> TsdlResult<String> {
    dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| TsdlError::Message(format!("Could not get dir name for {}", dir.display())))
}

/// Extract grammar name from directory (strips "tree-sitter-" prefix if present)
fn extract_grammar_name(dir: &Path) -> TsdlResult<GrammarName> {
    let dir_name = extract_dir_name(dir)?;
    let name = dir_name.strip_prefix("tree-sitter-").unwrap_or(&dir_name);
    Ok(GrammarName::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    use crate::{
        actors::{DisplayActor, DisplayAddr},
        args::{Target, TreeSitter},
        display::Mode,
        git,
    };
    use tempfile::TempDir;

    const FULL_SHA: &str = "636801770eea172d140e64b691815ff11f6b556f";

    #[test]
    fn ref_classifies_stable_refs() {
        assert!(Ref::parse("0.21.0").unwrap().is_stable());
        assert!(Ref::parse("v0.21.0").unwrap().is_stable());
        assert!(Ref::parse("refs/tags/release").unwrap().is_stable());
        assert!(Ref::parse(FULL_SHA).unwrap().is_stable());
    }

    #[test]
    fn ref_classifies_moving_refs() {
        assert!(Ref::head().is_moving());
        assert!(Ref::parse("master").unwrap().is_moving());
        assert!(Ref::parse("refs/heads/main").unwrap().is_moving());
        assert!(Ref::parse("vnext").unwrap().is_moving());
    }

    #[test]
    fn ref_serializes_as_requested_ref() {
        #[derive(serde::Serialize)]
        struct Wrapper {
            git_ref: Ref,
        }

        let source = Ref::parse("0.21.0").unwrap();

        assert_eq!(
            toml::to_string(&Wrapper { git_ref: source }).unwrap(),
            "git_ref = \"v0.21.0\"\n"
        );
    }

    async fn test_grammar_build(
        grammar_dir: PathBuf,
        out_dir: PathBuf,
        overwrite_output: bool,
    ) -> (GrammarBuild, DisplayAddr) {
        let display = DisplayActor::spawn(
            Mode::Plain,
            Arc::new(grammar_dir.clone()),
            Arc::new(out_dir.clone()),
        );
        let progress = display
            .add_grammar(git::Ref::head(), "rust", "rust", 1)
            .await;
        let source_ref = Ref::parse("v1.0.0").unwrap();
        let build = GrammarBuild {
            context: BuildContext { overwrite_output },
            cache_decision: CacheDecision::miss(cache::CacheMissReason::MissingEntry),
            dir: grammar_dir.clone().into(),
            hash: cache::GrammarHash::from("test"),
            language: "rust".into(),
            name: "rust".into(),
            output: OutputConfig {
                build_dir: grammar_dir.into(),
                out_dir: out_dir.into(),
            },
            progress,
            source: cache::Source::stable(),
            spec: Arc::new(BuildSpec {
                build_script: None,
                git_ref: source_ref,
                prefix: String::new(),
                repo: "https://example.com/tree-sitter-rust".parse().unwrap(),
                target: Target::Native,
                tree_sitter: TreeSitter::default(),
            }),
            ts_cli: Arc::new(PathBuf::from("tree-sitter-macos-arm64-v0.26.5")),
        };

        (build, display)
    }

    async fn write_artifact(build: &GrammarBuild, kind: ArtifactKind, contents: &[u8]) -> PathBuf {
        let path = build.artifact_path(kind).unwrap();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, contents).await.unwrap();
        path
    }

    fn same_identity(a: &Path, b: &Path) -> bool {
        let a = std::fs::metadata(a).unwrap();
        let b = std::fs::metadata(b).unwrap();
        same_file_identity(&a, &b)
    }

    #[test]
    fn test_cache_key_format() {
        let key = CacheKey::new(
            &LanguageName::from("typescript"),
            &GrammarName::from("typescript"),
        );
        assert_eq!(key.as_str(), "typescript/typescript");

        let key = CacheKey::new(&LanguageName::from("typescript"), &GrammarName::from("tsx"));
        assert_eq!(key.as_str(), "typescript/tsx");
    }

    #[test]
    fn test_extract_grammar_name() {
        let dir = Path::new("/tmp/build/tree-sitter-typescript");
        let name = extract_grammar_name(dir).unwrap();
        assert_eq!(name.as_str(), "typescript");

        let dir = Path::new("/tmp/build/custom-parser");
        let name = extract_grammar_name(dir).unwrap();
        assert_eq!(name.as_str(), "custom-parser");
    }

    #[test]
    fn test_extract_grammar_name_strips_prefix() {
        let name_with_prefix = "tree-sitter-typescript";
        let stripped = name_with_prefix
            .strip_prefix("tree-sitter-")
            .unwrap_or(name_with_prefix);
        assert_eq!(stripped, "typescript");

        let name_without_prefix = "custom-parser";
        let not_stripped = name_without_prefix
            .strip_prefix("tree-sitter-")
            .unwrap_or(name_without_prefix);
        assert_eq!(not_stripped, "custom-parser");
    }

    #[test]
    fn test_artifact_dir_name_from_tree_sitter_cli() {
        assert_eq!(
            artifact_dir_name_from_tree_sitter_cli(Path::new(
                "/tmp/tsdl/tree-sitter-macos-arm64-v0.26.5"
            ))
            .unwrap(),
            "tsdl-macos-arm64-v0.26.5"
        );
    }

    #[test]
    fn test_parser_name_and_ext() {
        let name = parser_name_and_ext("", &GrammarName::from("typescript"), ArtifactKind::Native);
        assert_eq!(name, format!("typescript.{DLL_EXTENSION}"));
    }

    #[test]
    fn test_parser_name_with_prefix() {
        let name = parser_name_and_ext("lib", &GrammarName::from("typescript"), ArtifactKind::Native);
        assert_eq!(name, format!("libtypescript.{DLL_EXTENSION}"));
    }

    #[tokio::test]
    async fn install_missing_destination_creates_hardlink() {
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("grammar");
        let out_dir = temp_dir.path().join("out");
        tokio::fs::create_dir_all(&grammar_dir).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let dst = out_dir.join(format!("rust.{DLL_EXTENSION}"));

        let (build, display) = test_grammar_build(grammar_dir, out_dir, false).await;
        let src = write_artifact(&build, ArtifactKind::Native, b"parser").await;
        build.install_binary(ArtifactKind::Native).await.unwrap();
        display.shutdown(false).await;

        assert!(same_identity(&src, &dst));
    }

    #[tokio::test]
    async fn install_same_content_relinks_without_force() {
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("grammar");
        let out_dir = temp_dir.path().join("out");
        tokio::fs::create_dir_all(&grammar_dir).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let dst = out_dir.join(format!("rust.{DLL_EXTENSION}"));

        let (build, display) = test_grammar_build(grammar_dir, out_dir, false).await;
        let src = write_artifact(&build, ArtifactKind::Native, b"parser").await;
        tokio::fs::write(&dst, b"parser").await.unwrap();
        assert!(!same_identity(&src, &dst));

        build.install_binary(ArtifactKind::Native).await.unwrap();
        display.shutdown(false).await;

        assert!(same_identity(&src, &dst));
    }

    #[tokio::test]
    async fn install_same_size_different_content_requires_force() {
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("grammar");
        let out_dir = temp_dir.path().join("out");
        tokio::fs::create_dir_all(&grammar_dir).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let dst = out_dir.join(format!("rust.{DLL_EXTENSION}"));

        let (build, display) = test_grammar_build(grammar_dir, out_dir, false).await;
        let src = write_artifact(&build, ArtifactKind::Native, b"new").await;
        tokio::fs::write(&dst, b"old").await.unwrap();
        let err = build
            .install_binary(ArtifactKind::Native)
            .await
            .unwrap_err();
        display.shutdown(false).await;

        assert!(err.to_string().contains("differs from the built parser"));
        assert_eq!(tokio::fs::read(&dst).await.unwrap(), b"old");
        assert!(!same_identity(&src, &dst));
    }

    #[tokio::test]
    async fn install_same_size_different_content_with_force_replaces() {
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("grammar");
        let out_dir = temp_dir.path().join("out");
        tokio::fs::create_dir_all(&grammar_dir).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let dst = out_dir.join(format!("rust.{DLL_EXTENSION}"));

        let (build, display) = test_grammar_build(grammar_dir, out_dir, true).await;
        let src = write_artifact(&build, ArtifactKind::Native, b"new").await;
        tokio::fs::write(&dst, b"old").await.unwrap();
        build.install_binary(ArtifactKind::Native).await.unwrap();
        display.shutdown(false).await;

        assert_eq!(tokio::fs::read(&dst).await.unwrap(), b"new");
        assert!(same_identity(&src, &dst));
    }

    #[tokio::test]
    async fn install_symlink_requires_force() {
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("grammar");
        let out_dir = temp_dir.path().join("out");
        tokio::fs::create_dir_all(&grammar_dir).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let dst = out_dir.join(format!("rust.{DLL_EXTENSION}"));
        let target = out_dir.join(format!("target.{DLL_EXTENSION}"));

        let (build, display) = test_grammar_build(grammar_dir, out_dir, false).await;
        let _src = write_artifact(&build, ArtifactKind::Native, b"new").await;
        tokio::fs::write(&target, b"old").await.unwrap();
        symlink(&target, &dst).unwrap();
        let err = build
            .install_binary(ArtifactKind::Native)
            .await
            .unwrap_err();
        display.shutdown(false).await;

        assert!(err.to_string().contains("symlink"));
        assert_eq!(std::fs::read_link(&dst).unwrap(), target);
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"old");
    }

    #[tokio::test]
    async fn install_directory_destination_errors_even_with_force() {
        let temp_dir = TempDir::new().unwrap();
        let grammar_dir = temp_dir.path().join("grammar");
        let out_dir = temp_dir.path().join("out");
        tokio::fs::create_dir_all(&grammar_dir).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let dst = out_dir.join(format!("rust.{DLL_EXTENSION}"));

        let (build, display) = test_grammar_build(grammar_dir, out_dir, true).await;
        let _src = write_artifact(&build, ArtifactKind::Native, b"new").await;
        tokio::fs::create_dir(&dst).await.unwrap();
        let err = build
            .install_binary(ArtifactKind::Native)
            .await
            .unwrap_err();
        display.shutdown(false).await;

        assert!(err.to_string().contains("directory"));
        assert!(dst.is_dir());
    }
}
