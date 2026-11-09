//! Fragment paths and test hooks.

use super::*;

// Cache successful metadata responses, including absent paths, for the process lifetime.
static FRAGMENT_PATH_CACHE: OnceLock<Mutex<FragmentPathCache>> = OnceLock::new();

#[derive(Default)]
struct FragmentPathCache {
    paths: std::collections::HashMap<String, String>,
    queried: std::collections::HashSet<String>,
}

fn fragment_path_cache() -> &'static Mutex<FragmentPathCache> {
    FRAGMENT_PATH_CACHE.get_or_init(|| Mutex::new(FragmentPathCache::default()))
}

pub(crate) fn batch_fragment_paths_via(
    names: &[String],
    deadline: Duration,
    cancellation: &CancelToken,
    run: &mut impl FnMut(ProcessRequest, &CancelToken) -> Result<ProcessOutput, ProcessError>,
) -> std::collections::HashMap<String, String> {
    if names.is_empty() {
        return std::collections::HashMap::new();
    }

    let (mut map, unknown) = {
        let Ok(cache) = fragment_path_cache().lock() else {
            // A poisoned lock must not blank the column: fall back to asking.
            return uncached_fragment_paths_via(names, deadline, cancellation, run)
                .unwrap_or_default();
        };
        let mut map = std::collections::HashMap::new();
        let mut unknown = Vec::new();
        for name in names {
            if let Some(path) = cache.paths.get(name) {
                map.insert(name.clone(), path.clone());
            } else if !cache.queried.contains(name) {
                unknown.push(name.clone());
            }
        }
        (map, unknown)
    };

    if unknown.is_empty() {
        return map;
    }

    let Some(fetched) = uncached_fragment_paths_via(&unknown, deadline, cancellation, run) else {
        return map;
    };
    if let Ok(mut cache) = fragment_path_cache().lock() {
        for name in &unknown {
            cache.queried.insert(name.clone());
        }
        #[allow(
            clippy::iter_over_hash_type,
            reason = "each entry updates a distinct cache key"
        )]
        for (name, path) in &fetched {
            cache.paths.insert(name.clone(), path.clone());
        }
    }
    map.extend(fetched);
    map
}

fn uncached_fragment_paths_via(
    names: &[String],
    deadline: Duration,
    cancellation: &CancelToken,
    run: &mut impl FnMut(ProcessRequest, &CancelToken) -> Result<ProcessOutput, ProcessError>,
) -> Option<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    if names.is_empty() {
        return Some(map);
    }
    let request = ProcessRequest::new(
        SYSTEMCTL_EXE,
        StdinPolicy::Null,
        FRAGMENT_STDOUT_MAX_BYTES,
        HELPER_STDERR_MAX_BYTES,
        deadline,
    )
    .arg("show")
    .args(names.iter().cloned())
    .args(["-p", "Id,FragmentPath", "--no-pager"]);
    let output = match run(request, cancellation) {
        // Failed or truncated responses must remain retryable and must not be parsed.
        Ok(output) if output.status.success() => output,
        _ => return None,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_id = String::new();
    let mut current_path = String::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !current_id.is_empty() && !current_path.is_empty() {
                map.insert(
                    std::mem::take(&mut current_id),
                    std::mem::take(&mut current_path),
                );
            }
            current_id.clear();
            current_path.clear();
            continue;
        }
        if let Some(value) = line.strip_prefix("Id=") {
            value.clone_into(&mut current_id);
        } else if let Some(value) = line.strip_prefix("FragmentPath=") {
            value.clone_into(&mut current_path);
        }
    }
    if !current_id.is_empty() && !current_path.is_empty() {
        map.insert(current_id, current_path);
    }
    Some(map)
}

// The idle-cost gate invokes the same collector as the owned worker so its
// counters do not depend on asynchronous publication timing.
