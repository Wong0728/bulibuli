//! 版本比较与平台资产匹配：解析 `v2.0.0-alpha.9` 形态的版本号并排序，
//! 从资产清单中选出与当前平台匹配的 portable 包。

use super::check::ReleaseAsset;
use std::cmp::Ordering;

/// 版本比较：v2.0.0-alpha.9 按 (major, minor, patch, 预发布类型, 预发布号) 比较，
/// 正式版大于任何预发布版。
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => {
            let core = x.0.cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2));
            match (x.3, y.3) {
                (None, None) => core,
                (None, Some(_)) => core.then(Ordering::Greater),
                (Some(_), None) => core.then(Ordering::Less),
                (Some(px), Some(py)) => core.then(px.cmp(&py)),
            }
        }
        _ => a.cmp(b),
    }
}

/// 解析后的版本号：核心三元组 + 可选预发布（类型序号 alpha<beta<rc，发布号）。
type ParsedVersion = (u64, u64, u64, Option<(u8, u64)>);

fn parse_version(version: &str) -> Option<ParsedVersion> {
    let version = version.trim().trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut nums = core.split('.');
    let major = nums.next()?.parse().ok()?;
    let minor = nums.next()?.parse().ok()?;
    let patch = nums.next().unwrap_or("0").parse().ok()?;
    let pre = pre.map(|pre| {
        // 无法识别的预发布后缀（nightly、dev、无序号 alpha 等）保守按最低档
        // （rank 0，早于 alpha）处理：宁可被当作"更旧的预发布"，也不当作
        // 正式版——否则 latest.json 里出现陌生后缀时会误判为正式发布，
        // 触发更新提示/自动下载的错误决策。
        let (kind, number) = pre.split_once('.').unwrap_or((pre, ""));
        let rank = match kind {
            "alpha" => 1,
            "beta" => 2,
            "rc" => 3,
            _ => 0,
        };
        (rank, number.parse().unwrap_or(0))
    });
    Some((major, minor, patch, pre))
}

/// 当前平台的 (platform, architecture, variant)，与 latest.json 资产字段对齐。
pub fn current_platform() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_arch = "x86_64")]
    let arch = "x86_64";
    #[cfg(target_arch = "aarch64")]
    let arch = "arm64";
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!("bulibuli 更新机制暂不支持当前 CPU 架构");

    #[cfg(target_os = "windows")]
    {
        ("windows", arch, "portable")
    }
    #[cfg(target_os = "macos")]
    {
        ("macos", arch, "portable")
    }
    #[cfg(target_os = "android")]
    {
        ("termux", "arm64", "portable")
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        ("linux", arch, "portable")
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "android",
        all(unix, not(target_os = "macos"), not(target_os = "android"))
    )))]
    {
        ("unknown", arch, "portable")
    }
}

/// 从资产清单里选当前平台匹配的 portable 包。
pub fn matching_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    let (platform, architecture, variant) = current_platform();
    assets.iter().find(|asset| {
        asset.platform == platform && asset.architecture == architecture && asset.variant == variant
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_orders_prereleases_and_releases() {
        assert_eq!(
            compare_versions("v2.0.0-alpha.9", "v2.0.0-alpha.8"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0-alpha.10", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0-beta.1", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.1-alpha.1", "v2.0.0"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("v2.0.0", "v2.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("v1.9.9", "v2.0.0"), Ordering::Less);
    }

    #[test]
    fn version_comparison_tolerates_unparseable_input() {
        // 无法解析时回退字符串比较，不会 panic。
        let _ordering = compare_versions("latest", "v2.0.0");
    }

    #[test]
    fn unrecognized_prerelease_suffixes_are_never_treated_as_release() {
        // 陌生后缀（nightly/dev/无序号 alpha）不得被当作正式版：与同核心版本
        // 的正式版比较必须更旧，与已知预发布比较也更旧（保守降档到 rank 0）。
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-nightly"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-alpha"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0", "v2.0.0-dev.3"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("v2.0.0-alpha.1", "v2.0.0-nightly"),
            Ordering::Greater
        );
        // 语义不变：正式版之间、已知预发布之间照常比较。
        assert_eq!(compare_versions("v2.0.0", "v2.0.0"), Ordering::Equal);
        assert_eq!(
            compare_versions("v2.0.0-beta.1", "v2.0.0-alpha.9"),
            Ordering::Greater
        );
    }
}
