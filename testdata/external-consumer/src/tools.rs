use crabber::AgentBuilder;
use crabber_tools_catalog::prelude::*;
use std::{path::Path, sync::Arc, time::Duration};
pub fn install(
    builder: AgentBuilder,
    a: &Path,
    b: &Path,
) -> Result<AgentBuilder, Box<dyn std::error::Error>> {
    let policies = Policies {
        enabled: EnabledSet::only([ToolId::FileRead, ToolId::FileList]),
        restrict_to_enabled: false,
        shell: ShellPolicy {
            shell_binary: "/bin/sh".into(),
            startup_mode: StartupMode::NonLogin,
            env: EnvPolicy::minimal_allowlist(),
            run_as: None,
            output_cap_bytes: 65536,
        },
        search: SearchPolicy::resolve_rg_from_path(EnvPolicy::minimal_allowlist())?,
        url_fetch: None,
        tracker: None,
        user_interact: None,
        limits: Limits {
            max_in_flight: 4,
            max_blocking_wait: Duration::from_secs(2),
        },
    };
    let roots = ExactRoots::new([("ws-a".into(), a.to_owned()), ("ws-b".into(), b.to_owned())])?;
    Ok(
        builder.extension_factory(Arc::new(StandardToolsFactory::new(
            policies,
            Arc::new(roots),
        )?)),
    )
}
