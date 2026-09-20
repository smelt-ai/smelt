//! One managed Bun process that loads shared plugins.
//!
//! Bun is intentionally not a sandbox. This host only removes per-package process overhead and
//! keeps the control protocol off stdout. User-installed packages run with the current user's
//! OS permissions.

use super::{
    HostError, PluginLifecycleState, PluginPackage, PluginProcessVerifier, PluginStatus,
    SpawnOptions, read_json_line, set_cloexec, set_cloexec_io, terminate_child, write_json_line,
};
use serde::{Deserialize, Serialize};
use smelt_plugin_api::{InvocationRequest, InvocationResponse, PluginContributionSet, PluginId};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::BufReader,
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

const SHARED_BUN_HOST_FD_ENV: &str = "SMELT_SHARED_BUN_HOST_FD";
const SHARED_BUN_PLUGIN_ENV: &str = "SMELT_SHARED_BUN_HOST";
const SHARED_BUN_RUNNER_DIR: &str = ".shared-bun-host";
const SHARED_BUN_RUNNER_FILE: &str = "runner.ts";
const SHARED_BUN_RUNNER: &str = include_str!("shared_bun_runner.ts");

#[derive(Clone, Debug, Default)]
pub struct SharedBunHostOptions {
    pub spawn: SpawnOptions,
    pub disabled: BTreeSet<PluginId>,
}

pub struct SharedBunHost {
    plugin_data_root: PathBuf,
    verifier: Arc<dyn PluginProcessVerifier>,
    options: SharedBunHostOptions,
    state: Mutex<SharedBunState>,
}

struct SharedBunState {
    plugins: BTreeMap<PluginId, SharedBunPlugin>,
    process: Option<SharedBunProcess>,
}

struct SharedBunPlugin {
    package: PluginPackage,
    disabled: bool,
    status: PluginStatus,
}

struct SharedBunProcess {
    child: Child,
    control: BufReader<UnixStream>,
    shutdown_grace: Duration,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostMessage {
    Init {
        plugins: Vec<BunPluginDescriptor>,
    },
    Invoke {
        plugin_id: String,
        request: InvocationRequest,
    },
    Shutdown,
}

#[derive(Serialize)]
struct BunPluginDescriptor {
    plugin_id: String,
    entrypoint: String,
    data_dir: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RunnerMessage {
    Ready {
        plugins: Vec<BunPluginLoad>,
    },
    InvocationResult {
        plugin_id: String,
        response: InvocationResponse,
    },
}

#[derive(Deserialize)]
struct BunPluginLoad {
    plugin_id: String,
    #[serde(default)]
    error: Option<String>,
}

impl SharedBunHost {
    pub fn start_packages(
        packages: Vec<PluginPackage>,
        plugin_data_root: PathBuf,
        verifier: Arc<dyn PluginProcessVerifier>,
        options: SharedBunHostOptions,
    ) -> Result<Self, HostError> {
        if !plugin_data_root.is_absolute() {
            return Err(HostError::new("plugin data root must be absolute"));
        }
        let mut plugins = BTreeMap::new();
        for package in packages {
            let id = package.manifest().id.clone();
            if plugins.contains_key(&id) {
                return Err(HostError::new(format!(
                    "duplicate plugin id rejected: {id}"
                )));
            }
            let disabled = options.disabled.contains(&id);
            plugins.insert(
                id,
                SharedBunPlugin {
                    status: plugin_status(
                        &package,
                        if disabled {
                            PluginLifecycleState::Disabled
                        } else {
                            PluginLifecycleState::Discovered
                        },
                        None,
                    ),
                    package,
                    disabled,
                },
            );
        }
        let host = Self {
            plugin_data_root,
            verifier,
            options,
            state: Mutex::new(SharedBunState {
                plugins,
                process: None,
            }),
        };
        let mut state = host
            .state
            .lock()
            .map_err(|_| HostError::new("shared bun host lock poisoned"))?;
        if let Err(error) = host.start_locked(&mut state) {
            mark_enabled_failed(&mut state, error.to_string());
        }
        drop(state);
        Ok(host)
    }

    /// 共享 Bun 子进程 pid；进程组与 pid 相同（spawn 时 `process_group(0)`）。
    pub fn process_id(&self) -> Option<u32> {
        let state = self.state.lock().ok()?;
        state.process.as_ref().map(|process| process.child.id())
    }

    pub fn statuses(&self) -> Vec<PluginStatus> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        self.refresh_process_locked(&mut state);
        state
            .plugins
            .values()
            .map(|plugin| plugin.status.clone())
            .collect()
    }

    pub fn contributions(&self) -> Vec<PluginContributionSet> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        self.refresh_process_locked(&mut state);
        state
            .plugins
            .values()
            .filter(|plugin| matches!(plugin.status.state, PluginLifecycleState::Ready { .. }))
            .map(|plugin| PluginContributionSet {
                plugin_id: plugin.package.manifest().id.clone(),
                name: plugin.package.manifest().name.clone(),
                version: plugin.package.manifest().version.clone(),
                contributions: plugin.package.manifest().contributions.clone(),
            })
            .filter(|set| !set.contributions.is_empty())
            .collect()
    }

    pub fn invoke(
        &self,
        plugin_id: PluginId,
        request: InvocationRequest,
        timeout: Duration,
    ) -> Result<InvocationResponse, HostError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HostError::new("shared bun host lock poisoned"))?;
        self.refresh_process_locked(&mut state);
        let package = state
            .plugins
            .get(&plugin_id)
            .ok_or_else(|| HostError::new("plugin is not installed"))?
            .package
            .clone();
        package.validate_invocation(&request)?;
        if state
            .plugins
            .get(&plugin_id)
            .is_some_and(|plugin| plugin.disabled)
        {
            return Err(HostError::new("plugin is disabled"));
        }
        if state.process.is_none() {
            mark_enabled_discovered(&mut state);
            if let Err(error) = self.start_locked(&mut state) {
                mark_enabled_failed(&mut state, error.to_string());
                return Err(error);
            }
        }
        if let Some(plugin) = state.plugins.get(&plugin_id)
            && !matches!(plugin.status.state, PluginLifecycleState::Ready { .. })
        {
            return Err(HostError::new(format!(
                "plugin is not ready: {}",
                plugin
                    .status
                    .last_error
                    .as_deref()
                    .unwrap_or("shared bun module failed to load")
            )));
        }
        let result = invoke_locked(&mut state, &plugin_id, request, timeout);
        if let Err(error) = &result {
            if let Some(mut process) = state.process.take() {
                process.stop();
            }
            mark_enabled_failed(&mut state, format!("shared bun host stopped: {error}"));
        }
        result
    }

    /// Bun modules have no unload hook that can be trusted to release all ambient resources.
    /// Restarting the one host after an enablement change gives deterministic unload semantics.
    pub fn set_enabled(&self, plugin_id: PluginId, enabled: bool) -> Result<(), HostError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HostError::new("shared bun host lock poisoned"))?;
        let plugin = state
            .plugins
            .get_mut(&plugin_id)
            .ok_or_else(|| HostError::new("plugin is not installed"))?;
        plugin.disabled = !enabled;
        plugin.status = plugin_status(
            &plugin.package,
            if enabled {
                PluginLifecycleState::Discovered
            } else {
                PluginLifecycleState::Disabled
            },
            None,
        );
        if let Some(mut process) = state.process.take() {
            process.stop();
        }
        if let Err(error) = self.start_locked(&mut state) {
            mark_enabled_failed(&mut state, error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn refresh_process_locked(&self, state: &mut SharedBunState) {
        let failure = match state.process.as_mut() {
            Some(process) => match process.child.try_wait() {
                Ok(Some(status)) => Some(format!("shared bun host exited with {status}")),
                Ok(None) => None,
                Err(error) => Some(format!("inspect shared bun host process: {error}")),
            },
            None => None,
        };
        if let Some(error) = failure {
            if let Some(mut process) = state.process.take() {
                process.stop();
            }
            mark_enabled_failed(state, error);
        }
    }

    fn start_locked(&self, state: &mut SharedBunState) -> Result<(), HostError> {
        if state.process.is_some() {
            return Ok(());
        }
        let Some(first_package) = state
            .plugins
            .values()
            .find(|plugin| !plugin.disabled)
            .map(|plugin| plugin.package.clone())
        else {
            return Ok(());
        };
        let runner = install_runner(&self.plugin_data_root)?;
        let plugin_data_root = self.plugin_data_root.canonicalize()?;
        let descriptors = state
            .plugins
            .values()
            .filter(|plugin| !plugin.disabled)
            .map(|plugin| {
                let entrypoint = plugin
                    .package
                    .entrypoint()
                    .to_str()
                    .ok_or_else(|| HostError::new("plugin entrypoint path must be valid UTF-8"))?
                    .to_string();
                let data_dir = prepare_shared_bun_plugin_data_dir(
                    &plugin_data_root,
                    &plugin.package.manifest().id,
                )?;
                let data_dir = data_dir
                    .to_str()
                    .ok_or_else(|| {
                        HostError::new("plugin data directory path must be valid UTF-8")
                    })?
                    .to_string();
                Ok(BunPluginDescriptor {
                    plugin_id: plugin.package.manifest().id.as_str().to_string(),
                    entrypoint,
                    data_dir,
                })
            })
            .collect::<Result<Vec<_>, HostError>>()?;
        let bun = self
            .options
            .spawn
            .bun
            .clone()
            .ok_or_else(|| HostError::new("plugin runtime bun is unavailable"))?;
        let (parent_control, child_control) = UnixStream::pair().map_err(|error| {
            HostError::new(format!("create shared bun control channel: {error}"))
        })?;
        set_cloexec(parent_control.as_raw_fd(), true)?;
        set_cloexec(child_control.as_raw_fd(), true)?;
        let child_fd = child_control.as_raw_fd();
        let mut command = Command::new(&bun);
        command
            .arg(&runner)
            .current_dir(&plugin_data_root)
            .env_clear()
            .env(SHARED_BUN_HOST_FD_ENV, child_fd.to_string())
            .env(SHARED_BUN_PLUGIN_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for name in ["HOME", "LANG", "LC_ALL", "PATH", "TMPDIR", "RUST_BACKTRACE"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        unsafe {
            command.pre_exec(move || set_cloexec_io(child_fd, false));
        }
        let mut child = command
            .spawn()
            .map_err(|error| HostError::new(format!("spawn shared bun host: {error}")))?;
        drop(child_control);
        let result = (|| {
            self.verifier.verify(&first_package, &bun, child.id())?;
            parent_control.set_read_timeout(Some(self.options.spawn.startup_timeout))?;
            parent_control.set_write_timeout(Some(self.options.spawn.startup_timeout))?;
            let mut control = BufReader::new(parent_control);
            write_json_line(
                &mut control.get_mut(),
                &HostMessage::Init {
                    plugins: descriptors,
                },
            )?;
            let RunnerMessage::Ready { plugins } = read_json_line(&mut control)? else {
                return Err(HostError::new(
                    "shared bun host sent an invocation result during startup",
                ));
            };
            apply_load_results(state, &plugins, child.id())?;
            control.get_mut().set_read_timeout(None)?;
            control.get_mut().set_write_timeout(None)?;
            Ok(control)
        })();
        match result {
            Ok(control) => {
                state.process = Some(SharedBunProcess {
                    child,
                    control,
                    shutdown_grace: self.options.spawn.shutdown_grace,
                });
                Ok(())
            }
            Err(error) => {
                terminate_child(&mut child, self.options.spawn.shutdown_grace);
                Err(error)
            }
        }
    }
}

impl Drop for SharedBunHost {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock()
            && let Some(mut process) = state.process.take()
        {
            process.stop();
        }
    }
}

impl SharedBunProcess {
    fn stop(&mut self) {
        let _ = write_json_line(self.control.get_mut(), &HostMessage::Shutdown);
        terminate_child(&mut self.child, self.shutdown_grace);
    }
}

fn invoke_locked(
    state: &mut SharedBunState,
    plugin_id: &PluginId,
    request: InvocationRequest,
    timeout: Duration,
) -> Result<InvocationResponse, HostError> {
    let process = state
        .process
        .as_mut()
        .ok_or_else(|| HostError::new("shared bun host is unavailable"))?;
    process.control.get_mut().set_read_timeout(Some(timeout))?;
    process.control.get_mut().set_write_timeout(Some(timeout))?;
    let result = (|| {
        write_json_line(
            process.control.get_mut(),
            &HostMessage::Invoke {
                plugin_id: plugin_id.as_str().to_string(),
                request: request.clone(),
            },
        )?;
        let RunnerMessage::InvocationResult {
            plugin_id: response_plugin_id,
            response,
        } = read_json_line(&mut process.control)?
        else {
            return Err(HostError::new(
                "shared bun host sent a startup message during invocation",
            ));
        };
        if response_plugin_id != plugin_id.as_str() {
            return Err(HostError::new(
                "shared bun host returned an invocation for another plugin",
            ));
        }
        if response.invocation_id() != &request.invocation_id {
            return Err(HostError::new(
                "shared bun host invocation response id does not match the request",
            ));
        }
        Ok(response)
    })();
    process.control.get_mut().set_read_timeout(None)?;
    process.control.get_mut().set_write_timeout(None)?;
    result
}

fn apply_load_results(
    state: &mut SharedBunState,
    results: &[BunPluginLoad],
    pid: u32,
) -> Result<(), HostError> {
    let mut by_id = BTreeMap::new();
    for result in results {
        if by_id
            .insert(result.plugin_id.as_str(), result.error.as_deref())
            .is_some()
        {
            return Err(HostError::new(
                "shared bun host returned duplicate plugin load results",
            ));
        }
    }
    for plugin in state.plugins.values_mut().filter(|plugin| !plugin.disabled) {
        let id = plugin.package.manifest().id.as_str();
        let Some(error) = by_id.remove(id) else {
            return Err(HostError::new(
                "shared bun host omitted a plugin load result",
            ));
        };
        plugin.status = match error {
            Some(error) => plugin_status(
                &plugin.package,
                PluginLifecycleState::Failed,
                Some(error.to_string()),
            ),
            None => plugin_status(&plugin.package, PluginLifecycleState::Ready { pid }, None),
        };
    }
    if !by_id.is_empty() {
        return Err(HostError::new(
            "shared bun host returned a load result for an unknown plugin",
        ));
    }
    Ok(())
}

fn mark_enabled_failed(state: &mut SharedBunState, error: String) {
    for plugin in state.plugins.values_mut().filter(|plugin| !plugin.disabled) {
        plugin.status = plugin_status(
            &plugin.package,
            PluginLifecycleState::Failed,
            Some(error.clone()),
        );
    }
}

fn mark_enabled_discovered(state: &mut SharedBunState) {
    for plugin in state.plugins.values_mut().filter(|plugin| !plugin.disabled) {
        plugin.status = plugin_status(&plugin.package, PluginLifecycleState::Discovered, None);
    }
}

fn plugin_status(
    package: &PluginPackage,
    state: PluginLifecycleState,
    last_error: Option<String>,
) -> PluginStatus {
    PluginStatus {
        plugin_id: package.manifest().id.clone(),
        name: package.manifest().name.clone(),
        version: package.manifest().version.clone(),
        state,
        last_error,
    }
}

fn prepare_shared_bun_plugin_data_dir(
    plugin_data_root: &Path,
    plugin_id: &PluginId,
) -> Result<PathBuf, HostError> {
    let data_dir = plugin_data_root.join(plugin_id.as_str());
    match fs::symlink_metadata(&data_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HostError::new(
                "shared bun plugin data directory must be a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&data_dir)?,
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))?;
    let data_dir = data_dir.canonicalize()?;
    if !data_dir.starts_with(plugin_data_root) {
        return Err(HostError::new(
            "shared bun plugin data directory escapes the plugin data root",
        ));
    }
    Ok(data_dir)
}

fn install_runner(plugin_data_root: &Path) -> Result<PathBuf, HostError> {
    fs::create_dir_all(plugin_data_root)?;
    let root_metadata = fs::symlink_metadata(plugin_data_root)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(HostError::new(
            "plugin data root for shared bun host must be a real directory",
        ));
    }
    fs::set_permissions(plugin_data_root, fs::Permissions::from_mode(0o700))?;
    let plugin_data_root = plugin_data_root.canonicalize()?;
    let runner_dir = plugin_data_root.join(SHARED_BUN_RUNNER_DIR);
    match fs::symlink_metadata(&runner_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HostError::new(
                "shared bun host directory must be a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&runner_dir)?,
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(&runner_dir, fs::Permissions::from_mode(0o700))?;
    let runner_dir = runner_dir.canonicalize()?;
    if !runner_dir.starts_with(&plugin_data_root) {
        return Err(HostError::new(
            "shared bun host directory escapes the plugin data root",
        ));
    }
    let runner = runner_dir.join(SHARED_BUN_RUNNER_FILE);
    super::atomic_write(&runner, SHARED_BUN_RUNNER.as_bytes())?;
    Ok(runner)
}
