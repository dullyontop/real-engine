//! The sample runner's context and step registry. Each work package that adds
//! steps owns one module here and adds one line to [`step`].

#![allow(dead_code)]

pub mod import;
pub mod runtime;
pub mod sha256;

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use re_compat::ClassRegistry;
use re_import::{ImportOptions, ImportReport};
use serde::Deserialize;
use serde_json::Value as J;

/// One check of an expectation file.
#[derive(Clone, Debug, Deserialize)]
pub struct Check {
    pub id: String,
    pub step: String,
    #[serde(default)]
    pub args: J,
}

/// The import options of an expectation file.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    #[serde(default)]
    pub keep_stale: bool,
}

/// An expectation file.
#[derive(Clone, Debug, Deserialize)]
pub struct Expect {
    pub name: String,
    pub input: String,
    pub input_sha256: String,
    #[serde(default)]
    pub import: ImportArgs,
    #[serde(default)]
    pub checks: Vec<Check>,
    /// Sections later work packages read (boot, stand-ins, drivers).
    #[serde(flatten)]
    pub extra: HashMap<String, J>,
}

impl Expect {
    pub fn read(file: &Path) -> Expect {
        let text =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
    }
}

/// What a check concluded.
pub enum Outcome {
    Pass,
    Fail(String),
    /// The step is not registered yet.
    Pending,
}

/// A step: checks `args` against the context.
pub type Step = fn(&mut Ctx, &J) -> Result<(), String>;

/// The steps, by name. Add one line per step module.
pub fn step(name: &str) -> Option<Step> {
    import::step(name).or_else(|| runtime::step(name))
}

/// The state a sample's checks share.
pub struct Ctx {
    pub kit: PathBuf,
    pub expect: Expect,
    pub classes: ClassRegistry,
    /// The temporary directory holding the imported project and variants.
    pub work: tempfile::TempDir,
    /// The import of the unmodified input, made on first use.
    pub imported: Option<Result<ImportReport, re_core::Diagnostic>>,
    /// State other step modules keep, by key.
    pub state: HashMap<String, Box<dyn Any>>,
}

impl Ctx {
    pub fn new(kit: &Path, expect: &Expect) -> Ctx {
        Ctx {
            kit: kit.to_path_buf(),
            expect: expect.clone(),
            classes: classes(),
            work: tempfile::tempdir().expect("a temporary directory"),
            imported: None,
            state: HashMap::new(),
        }
    }

    /// A path from the kit (relative to the kit directory).
    pub fn kit_path(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.kit.join(p)
        }
    }

    pub fn input(&self) -> PathBuf {
        self.kit_path(&self.expect.input)
    }

    /// The imported project's directory.
    pub fn project(&self) -> PathBuf {
        self.work.path().join("project")
    }

    pub fn options(&self) -> ImportOptions {
        ImportOptions {
            dry_run: false,
            keep_stale: self.expect.import.keep_stale,
        }
    }

    /// The import of the unmodified input into [`Ctx::project`].
    pub fn import(&mut self) -> Result<&ImportReport, String> {
        if self.imported.is_none() {
            let input = self.input();
            let bytes = std::fs::read(&input).map_err(|e| format!("cannot read the input: {e}"))?;
            let hash = sha256::hex(&bytes);
            if !hash.starts_with(&self.expect.input_sha256) {
                return Err(format!(
                    "the input's sha256 is {hash}, not {}...",
                    self.expect.input_sha256
                ));
            }
            let opts = self.options();
            let project = self.project();
            self.imported = Some(re_import::import(&input, &project, &self.classes, &opts));
        }
        match self.imported.as_ref().expect("imported") {
            Ok(r) => Ok(r),
            Err(d) => Err(format!("the import failed: {}", d.render_human())),
        }
    }

    pub fn run(&mut self, check: &Check) -> Outcome {
        let Some(f) = step(&check.step) else {
            return Outcome::Pending;
        };
        match f(self, &check.args) {
            Ok(()) => Outcome::Pass,
            Err(m) => Outcome::Fail(m),
        }
    }
}

/// The runtime's class registry: the built-in data, with the converters and
/// accessors the runtime registers as work packages add them.
pub fn classes() -> ClassRegistry {
    ClassRegistry::builtin().expect("the built-in class data loads")
}
