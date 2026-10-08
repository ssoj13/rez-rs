// SPDX-License-Identifier: Apache-2.0

//! No-op build system for packages requiring no build step.

use std::path::PathBuf;

use super::{BuildContext, BuildResult, BuildSystem, BuildSystemType};
use crate::errors::Result;

/// No-op build system for packages that require no build step.
///
/// Used for pure-data packages, configs, etc.
#[derive(Debug, Clone)]
pub struct NoOpBuildSystem {
    pub working_dir: PathBuf,
}

impl NoOpBuildSystem {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

impl BuildSystem for NoOpBuildSystem {
    fn name(&self) -> &str {
        "noop"
    }
    fn build_type(&self) -> BuildSystemType {
        BuildSystemType::NoOp
    }
    fn is_valid(&self) -> bool {
        true
    }

    fn build(&self, ctx: &BuildContext) -> Result<BuildResult> {
        let mut result = BuildResult::ok(ctx.build_path.clone(), 0.0);
        if ctx.install {
            result.install_path = Some(ctx.install_path.clone());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_noop_build() {
        let noop = NoOpBuildSystem::new(PathBuf::from("/tmp"));
        assert!(noop.is_valid());
        assert_eq!(noop.name(), "noop");

        let ctx = BuildContext::new(
            PathBuf::from("/src"),
            PathBuf::from("/build"),
            PathBuf::from("/install"),
        );
        let result = noop.build(&ctx).expect("noop build should succeed");
        assert!(result.success);
        assert_eq!(result.elapsed_secs, 0.0);
    }

    #[test]
    fn test_noop_install() {
        let noop = NoOpBuildSystem::new(PathBuf::from("/tmp"));
        let mut ctx = BuildContext::new(
            PathBuf::from("/src"),
            PathBuf::from("/build"),
            PathBuf::from("/install"),
        );
        ctx.install = true;
        let result = noop.build(&ctx).expect("noop install should succeed");
        assert!(result.success);
        assert_eq!(result.install_path, Some(PathBuf::from("/install")));
    }
}
