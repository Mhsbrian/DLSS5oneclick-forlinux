//! The install steps, in the order the DLSS5-Feeder README lists them.
//!
//! Sources (verified 2026-08-31):
//! 0. dgVoodoo 2.87.3 — only when the game is Direct3D 9 and dgVoodoo is not
//!    already in the game folder. Downloaded from the official GitHub release
//!    (not bundled); extracts `MS/{x86|x64}/D3D9.dll` by exe bitness → `d3d9.dll`
//!    + smart-merged conf (force OutputAPI, floor VRAM, preserve the rest).
//! 1. ReShade add-on build — https://reshade.me links `/downloads/ReShade_Setup_<ver>_Addon.exe`;
//!    that exe has an appended ZIP with ReShade64.dll / ReShade32.dll. Dropped as dxgi.dll.
//! 2. ReShade shader headers — raw.githubusercontent.com/crosire/reshade-shaders/slim/Shaders/
//!    {ReShade.fxh, ReShadeUI.fxh, DrawText.fxh}; the setup exe only carries the DLLs.
//! 3. DLSS5-Feeder — jlrouzies-fr/DLSS5-Feeder latest release zip only
//!    (`dlss5-feed.addon64` + `DLSS5_Feed.fx`; `feed-vk-layer.zip` is Vulkan-only, unused).
//!    No local overwrite of Feeder binaries or shaders — official release assets only.
//! 4. LumeniteFX — umar-afzaal/LumeniteFX branch `mainline` (no releases):
//!    Shaders/lumenite_*.fx, Shaders/include/*.fxh, Textures/lumenite_bluenoise256.png.
//! 5. DLSS 5 add-on — RankFTW/rhi-repo releases: `renodx-dlss5-*` (renodx-dlss5.addon64),
//!    `dlssnr-*` (nvngx_dlssnr.dll), `dlss-*` (nvngx_dlss.dll; not dlssg-/dlssd-).
//! 6. ReShade.ini + ReShadePreset.ini: DLSS5_MV_PROVIDER=3, Lumenite_Kernel above DLSS5_Feed.
//!    Optional LUMENITE: TRAA stays user-controlled; we soft-patch UI protect + preset defaults.

use crate::game::{self, GameStatus};
use crate::gpu;
use crate::gpupref;
use crate::net::{self, Progress};
use crate::quality_preset::{self, QualityChoice, QualityOverrides, ResolvedQuality};
use crate::renodx;
use crate::reshade_ini;
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const RESHADE_HOME: &str = "https://reshade.me";
pub const RESHADE_SHADERS_RAW: &str =
    "https://raw.githubusercontent.com/crosire/reshade-shaders/slim/Shaders/";
pub const FEEDER_REPO: &str = "jlrouzies-fr/DLSS5-Feeder";
pub const LUMENITE_ZIP: &str =
    "https://codeload.github.com/umar-afzaal/LumeniteFX/zip/refs/heads/mainline";

/// Official dgVoodoo 2.87.3 release zip (not bundled — downloaded into the game
/// folder at Install time). License allows shipping individual DLLs with a
/// game; forbids bundling inside launchers for general multi-app use.
pub const DGVOODOO_TAG: &str = "v2.87.3";
pub const DGVOODOO_ZIP: &str =
    "https://github.com/dege-diosg/dgVoodoo2/releases/download/v2.87.3/dgVoodoo2_87_3.zip";
/// Zip members for D3D9 (32-bit Gothic-class vs rare 64-bit DX9).
const DGVOODOO_D3D9_MEMBER_X86: &str = "MS/x86/D3D9.dll";
const DGVOODOO_D3D9_MEMBER_X64: &str = "MS/x64/D3D9.dll";

/// Minimum emulated VRAM (MB). Stock dgVoodoo is 256 — too low for Gothic 3 VH @ 1080p.
const DGVOODOO_VRAM_FLOOR: u32 = 4096;
/// Feeder/ReShade need D3D11; never leave `bestavailable` (may pick D3D12).
const DGVOODOO_OUTPUT_API: &str = "d3d11_fl11_0";

/// Full template used only when no `dgVoodoo.conf` exists yet.
const DGVOODOO_CONF_TEMPLATE: &str = "\
; Written by DLSS5oneclick — official dgVoodoo 2.87.3 (DX9 → D3D11 for ReShade dxgi.dll)
; https://github.com/dege-diosg/dgVoodoo2/releases/tag/v2.87.3
[General]
OutputAPI = d3d11_fl11_0
Adapters = all
FullScreenOutput = default
ScalingMode = unspecified
[DirectX]
VideoCard = geforce_9800_gt
VRAM = 4096
Filtering = appdriven
Mipmapping = appdriven
Resolution = unforced
Antialiasing = appdriven
AppControlledScreenMode = true
ForceVerticalSync = false
dgVoodooWatermark = false
FastVideoMemoryAccess = false
[DirectXExt]
RTTexturesForceScaleAndMSAA = false
";

/// Marker embedded in the Lumenite TRAA UI-protect patch (idempotent).
const TRAA_UI_MARKER: &str = "DLSS5_TRAA_UI_PROTECT";
const TRAA_FX: &str = "lumenite_TRAA.fx";

/// Soft-patch installed `lumenite_TRAA.fx`: Geometric DLAA by default + skip temporal
/// blend where `DLSS5_Mask` / HUD-like luma edges without depth structure say so.
/// Leaves TRAA enabled/disabled as the user set it; only improves UI/text when on.
fn apply_traa_ui_patch(game_dir: &Path) -> Result<Option<String>> {
    let dest = game_dir
        .join("reshade-shaders")
        .join("Shaders")
        .join(TRAA_FX);
    if !dest.is_file() {
        return Ok(None);
    }
    let mut text =
        fs::read_to_string(&dest).with_context(|| format!("reading {}", dest.display()))?;
    if text.contains(TRAA_UI_MARKER) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect, already applied)"
        )));
    }

    // Default Edge Detection → Geometric (stock tooltip already says it ignores flat UI).
    let edge_anchor = "\"Geometric: silhouettes only, ignores flat UI.\";\n    > = 0;";
    if !text.contains(edge_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: EDGE_MODE layout changed)"
        )));
    }
    text = text.replace(
        edge_anchor,
        "\"Geometric: silhouettes only, ignores flat UI.\";\n    > = 1;",
    );

    let uniforms = r#"
// DLSS5_TRAA_UI_PROTECT -- favour current frame on HUD/text (DLSS5oneclick)
uniform bool UI_PROTECT <
    ui_label = "Protect UI / text (skip temporal)";
    ui_tooltip = "Lowers temporal blend where DLSS5_Mask distrusts motion, and where\n"
                 "sharp luma edges lack geometric depth/normal structure (typical HUD/text).\n"
                 "Needs Kernel above + DLSS 5 Feed above this effect for the bias mask.";
> = true;

uniform float UI_PROTECT_STRENGTH <
    ui_type = "drag";
    ui_min = 0.0; ui_max = 1.0; ui_step = 0.05;
    ui_label = "UI protect strength";
> = 1.0;

"#;
    let imports_anchor = "/*--------------.\n| :: IMPORTS :: |\n'--------------*/";
    if !text.contains(imports_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: IMPORTS layout changed)"
        )));
    }
    text = text.replace(imports_anchor, &format!("{uniforms}{imports_anchor}"));

    let mask_tex = r#"
// DLSS5_TRAA_UI_PROTECT -- same pooled mask Feed writes (bias-current-colour)
texture DLSS5_Mask { Width = BUFFER_WIDTH; Height = BUFFER_HEIGHT; Format = R8; };
sampler sDLSS5_Mask { Texture = DLSS5_Mask; MinFilter = POINT; MagFilter = POINT; MipFilter = POINT; };

"#;
    let ns_anchor = "namespace LumeniteTRAA {";
    if !text.contains(ns_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: namespace layout changed)"
        )));
    }
    text = text.replace(ns_anchor, &format!("{mask_tex}{ns_anchor}"));

    let conf_anchor = "    confidence = saturate(confidence + 0.11 * 4.0 * confidence * (1.0 - confidence));\n\n    float2 historyUV = texcoord + flow;";
    let conf_patch = r#"    confidence = saturate(confidence + 0.11 * 4.0 * confidence * (1.0 - confidence));

    // DLSS5_TRAA_UI_PROTECT
    if (UI_PROTECT)
    {
        float distrust = tex2Dlod(sDLSS5_Mask, float4(texcoord, 0.0, 0.0)).x;
        float4 nPack = tex2Dlod(Kernel::sNormals, float4(texcoord, 0.0, 0.0));
        float2 px = BUFFER_PIXEL_SIZE;
        float dL = tex2Dlod(Kernel::sNormals, float4(texcoord - float2(px.x, 0.0), 0.0, 0.0)).a;
        float dR = tex2Dlod(Kernel::sNormals, float4(texcoord + float2(px.x, 0.0), 0.0, 0.0)).a;
        float dT = tex2Dlod(Kernel::sNormals, float4(texcoord - float2(0.0, px.y), 0.0, 0.0)).a;
        float dB = tex2Dlod(Kernel::sNormals, float4(texcoord + float2(0.0, px.y), 0.0, 0.0)).a;
        float depthEdge = abs(dL - dR) + abs(dT - dB);
        float nEdge = length(nPack.xyz - tex2Dlod(Kernel::sNormals, float4(texcoord + float2(px.x, 0.0), 0.0, 0.0)).xyz);
        float lumaEdge = abs(GetLuminance(samples[3]) - GetLuminance(samples[5]))
                       + abs(GetLuminance(samples[1]) - GetLuminance(samples[7]));
        // Sharp text/HUD edges without geometric structure
        float uiHint = saturate(lumaEdge * 6.0) * (1.0 - saturate(depthEdge * 40.0 + nEdge * 4.0));
        // Screen-space UI often gets camera/scene flow while depth stays flat
        float mvPx = length(flow * float2(BUFFER_WIDTH, BUFFER_HEIGHT));
        float badFlow = saturate(mvPx * 0.25) * (1.0 - saturate(depthEdge * 40.0));
        float skip = saturate(max(max(distrust, uiHint), badFlow) * UI_PROTECT_STRENGTH);
        confidence *= (1.0 - skip);
    }

    float2 historyUV = texcoord + flow;"#;
    if !text.contains(conf_anchor) {
        return Ok(Some(format!(
            "reshade-shaders/Shaders/{TRAA_FX} (UI protect skipped: PS_TRAA layout changed)"
        )));
    }
    text = text.replace(conf_anchor, conf_patch);

    text = text.replace(
        "ui_tooltip = \"Temporal Reprojection Anti-Aliasing.\";",
        "ui_tooltip = \"Temporal Reprojection Anti-Aliasing.\\n\\n\
Place BELOW DLSS 5 Feed. Edge Detection=Geometric + Protect UI/text reduce HUD smear.\\n\
Uses DLSS5_Mask from Feed when present (DLSS5oneclick UI protect patch).\";",
    );

    fs::write(&dest, text).with_context(|| format!("writing patched {}", dest.display()))?;
    Ok(Some(format!(
        "reshade-shaders/Shaders/{TRAA_FX} (UI protect patch)"
    )))
}

const VULKAN_SETUP_TXT: &str = "\
DLSS5oneclick — Vulkan Feeder kit (manual finish)
================================================
This tool does NOT register ReShade as a Vulkan layer (that is why full
Install is refused). Files copied here still need ReShade's own setup.

1. Run ReShade Setup → select this game exe → choose Vulkan → Addon support.
2. In ReShade.ini next to the exe, under [ADDON]:
     AddonPath=<this folder>
3. Ensure dlss5-feed.addon64 and reshade-shaders/Shaders/DLSS5_Feed.fx are here
   (already copied by «Copy Vulkan Feeder kit»).
4. Also place a neural consumer (renodx-dlss5.addon64 + nvngx_dlssnr.dll) as for
   a 64-bit D3D game, or use Deep Fried Chicken per Feeder docs.
5. If dlss5-feed.log reports missing interop entry points, start the game via
   run-with-feed-layer.bat from the DLSS5-Feeder repo layer/ folder.

Do not expect dxgi.dll from this tool to load under Vulkan.
";

/// Drop Feeder addon + FX + setup note for manual Vulkan ReShade (no layer install).
/// Fetches the official DLSS5-Feeder release zip — never a bundled/modified add-on.
pub fn copy_vulkan_feeder_kit(game_dir: &Path) -> Result<Vec<String>> {
    let client = net::client()?;
    let tag = match net::latest_tag(&client, FEEDER_REPO) {
        Ok(t) => t,
        Err(_) => net::github_release_tags_html(&client, FEEDER_REPO, "v", 1)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no DLSS5-Feeder release found"))?,
    };
    let url = net::github_asset_url_html(&client, FEEDER_REPO, &tag, r#"[^"]+\.zip"#)?;
    let work = tempfile::tempdir()?;
    let zip_path = work.path().join("dlss5-feeder.zip");
    net::download(&client, &url, &zip_path, "DLSS5-Feeder", &|_, _| {})?;
    copy_vulkan_feeder_kit_from_zip(&zip_path, game_dir, &tag)
}

/// Extract official Feeder addon + FX from a release zip (testable offline).
pub fn copy_vulkan_feeder_kit_from_zip(
    zip_path: &Path,
    game_dir: &Path,
    tag: &str,
) -> Result<Vec<String>> {
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("DLSS5-Feeder download is not a valid zip")?;
    let members: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let pick = |want: &str| -> Option<String> {
        members
            .iter()
            .find(|m| net::file_name(&m.replace('\\', "/")).eq_ignore_ascii_case(want))
            .cloned()
    };
    let addon = pick(game::FEEDER_ADDON)
        .ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {}", game::FEEDER_ADDON))?;
    let fx = pick(game::FEEDER_FX)
        .ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {}", game::FEEDER_FX))?;
    net::extract_member(&mut zip, &addon, &game_dir.join(game::FEEDER_ADDON))?;
    net::extract_member(
        &mut zip,
        &fx,
        &game_dir
            .join("reshade-shaders")
            .join("Shaders")
            .join(game::FEEDER_FX),
    )?;
    let note = game_dir.join("VULKAN-SETUP.txt");
    fs::write(&note, VULKAN_SETUP_TXT).with_context(|| format!("writing {}", note.display()))?;
    Ok(vec![
        format!("{} ({tag})", game::FEEDER_ADDON),
        format!("reshade-shaders/Shaders/{}", game::FEEDER_FX),
        "VULKAN-SETUP.txt".into(),
    ])
}

/// Which install engine carries the DLSS 5 pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// ReShade + RenoDX add-on (both game kinds; the default).
    #[default]
    ReShade,
    /// Dagherbou's OptiScaler fork with the built-in Neural Rendering pass.
    /// Games with native DLSS only (the pass reads the inputs the game hands to DLSS).
    Opti,
}

const STEP_OPTI: Step = Step {
    name: "OptiScaler + DLSS Neural Rendering",
    run: step_opti,
};
const STEP_OPTI_FG: Step = Step {
    name: "OptiScaler frame generation (FSR 3.1)",
    run: step_opti_fg,
};

/// Extract the whole OptiScaler_DLSSNR release into the game folder,
/// writing `OptiScaler.dll` as `dxgi.dll` (the fork's default load name for
/// DX11/DX12 games) and recording every path in a manifest for uninstall.
/// The release tag recorded in an OptiScaler manifest, from its `# tag v…`
/// header. A manifest written before this was recorded has none.
/// The newest release of every component, fetched once and compared against
/// what each game has recorded. Empty fields mean "could not check".
#[derive(Debug, Clone, Default)]
pub struct Latest {
    pub reshade: Option<String>,
    pub feeder: Option<String>,
    pub opti: Option<String>,
    /// wilsjo2's pre-SR fork numbers its releases on its own, so its newest tag
    /// has to be carried separately from the stable build's.
    pub opti_presr: Option<String>,
    pub dlss: Option<String>,
    pub dlssnr: Option<String>,
}

impl Latest {
    pub fn fetch(client: &Client) -> Self {
        Latest {
            reshade: resolve_reshade_setup(client).ok().map(|(v, _)| v),
            feeder: net::latest_tag(client, FEEDER_REPO).ok(),
            opti: net::latest_tag(client, OPTI_REPO).ok(),
            opti_presr: net::latest_tag(client, OPTI_PRESR_REPO).ok(),
            dlss: rhi_latest(client, "dlss-").ok().map(|(t, _)| t),
            dlssnr: rhi_latest(client, "dlssnr-").ok().map(|(t, _)| t),
        }
    }
}

/// Files that must exist after a successful Feeder/Native install.
/// Used so the UI never says "Everything is in place" on a partial copy. A
/// Remix install is judged by its own two pieces instead.
pub fn missing_install_files(st: &GameStatus) -> Vec<String> {
    let mut missing = Vec::new();
    // The Remix route places none of the ReShade/Feeder set: its install is the
    // model inside `.trex/` and the neural pass switched on in rtx.conf.
    if st.remix.is_some() {
        if !st.remix_model {
            missing.push(format!(".trex/{}", game::DLSSNR_DLL));
        }
        if !st.remix_enabled {
            missing.push(
                "a neural pass enabled in rtx.conf (a runtime without one needs the runtime swap)"
                    .into(),
            );
        }
        return missing;
    }
    match st.mode {
        game::Mode::Feeder => {
            if !st.reshade {
                missing.push(format!("{} (ReShade)", game::RESHADE_PROXY));
            }
            if !st.headers {
                missing.push("reshade-shaders/Shaders headers (ReShade.fxh…)".into());
            }
            if !st.feeder {
                let addon = if st.is32() {
                    game::FEEDER_ADDON32
                } else {
                    game::FEEDER_ADDON
                };
                missing.push(format!("{addon} / {}", game::FEEDER_FX));
            }
            if !st.lumenite {
                missing.push("LumeniteFX shaders".into());
            }
            if !st.dlss5_addon && !st.upstream {
                missing.push("DLSS 5 neural consumer add-on".into());
            }
            if !st.dlssnr {
                missing.push(game::DLSSNR_DLL.into());
            }
            if !st.dlss {
                missing.push(game::DLSS_DLL.into());
            }
            if st.is32() {
                if !st.host_exe {
                    missing.push(format!("{}/{}", game::HOST_DIR, game::HOST_EXE));
                }
                if !st.host_reshade {
                    missing.push(format!("{}/{}", game::HOST_DIR, game::RESHADE_PROXY));
                }
            }
        }
        game::Mode::Native => {
            if st.opti {
                if !st.dlssnr {
                    missing.push(game::DLSSNR_DLL.into());
                }
            } else {
                if !st.reshade {
                    missing.push(format!("{} (ReShade)", game::RESHADE_PROXY));
                }
                if !(st.dlss5_addon || st.upstream) {
                    missing.push("DLSS 5 neural consumer add-on".into());
                }
                if !st.dlssnr {
                    missing.push(game::DLSSNR_DLL.into());
                }
                if st.needs_bridge() && !st.bridge {
                    missing.push("dx11 bridge add-on".into());
                }
            }
        }
    }
    missing
}

/// Components this tool placed in `dir` whose recorded version is behind
/// `latest`. A component with no marker was not placed by this tool and is
/// never reported, so a user's own ReShade never shows up as "out of date".
pub fn stale_components(dir: &Path, latest: &Latest) -> Vec<String> {
    let mine = |marker: &str| fs::read_to_string(dir.join(marker)).ok();
    let mut out = Vec::new();
    let mut check = |name: &str, have: Option<String>, want: &Option<String>| {
        if let (Some(h), Some(w)) = (have, want) {
            if h.trim() != w.trim() {
                out.push(format!("{name} {} → {w}", h.trim()));
            }
        }
    };
    check("ReShade", mine(game::RESHADE_MARKER), &latest.reshade);
    check("DLSS5-Feeder", mine(game::FEEDER_MARKER), &latest.feeder);
    check("nvngx_dlss.dll", mine(game::DLSS_MARKER), &latest.dlss);
    check(
        "nvngx_dlssnr.dll",
        mine(game::DLSSNR_MARKER),
        &latest.dlssnr,
    );
    if let Ok(m) = fs::read_to_string(dir.join(game::OPTI_MANIFEST)) {
        // Compared against the newest release of the fork it came from: the two
        // builds number their releases independently, so a pre-SR tag (v0.7.7)
        // measured against the stable build's (v0.2.0-dlssnr) reported an update
        // on every run that Install could never clear (#88).
        let want = if manifest_repo(&m) == OPTI_PRESR_REPO {
            &latest.opti_presr
        } else {
            &latest.opti
        };
        match (manifest_tag(&m), want) {
            (Some(have), Some(want)) if have.trim() != want.trim() => {
                out.push(format!("OptiScaler {} → {want}", have.trim()))
            }
            // Installed before the version was recorded (0.11.0), so what is on
            // disk cannot be compared: an Install settles it either way.
            (None, Some(want)) => out.push(format!("OptiScaler unknown version → {want}")),
            _ => {}
        }
    }
    out
}

fn manifest_tag(manifest: &str) -> Option<String> {
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("# tag "))
        .map(|t| t.trim().to_owned())
}

/// The repository an OptiScaler manifest's build came from: its `# repo`
/// header, or, in a manifest written before that header existed, the shape of
/// its tag -- the stable build's tags all end in "-dlssnr" and the pre-SR
/// fork's do not (#88). No tag at all reads as the stable build, the default.
fn manifest_repo(manifest: &str) -> &str {
    if let Some(repo) = manifest.lines().find_map(|l| l.strip_prefix("# repo ")) {
        return repo.trim();
    }
    match manifest_tag(manifest) {
        Some(t) if !t.ends_with("-dlssnr") => OPTI_PRESR_REPO,
        _ => OPTI_REPO,
    }
}

/// True when the OptiScaler this tool placed in `dir` is the pre-SR fork, so
/// the GUI opens on the build a game already has and an update keeps it.
pub fn installed_opti_presr(dir: &Path) -> bool {
    fs::read_to_string(game::join_ci(dir, &[game::OPTI_MANIFEST]))
        .is_ok_and(|m| manifest_repo(&m) == OPTI_PRESR_REPO)
}

/// The first stable release carrying a `.zip`. Both forks also publish rolling
/// "nightly" releases whose assets are `.7z`, and taking a release's first
/// asset blindly picked a checksum text file or an archive the installer
/// cannot open.
fn pick_opti_zip(releases: &[Value]) -> Option<String> {
    releases
        .iter()
        .filter(|r| r["prerelease"] != Value::Bool(true))
        .find_map(|r| {
            r.get("assets")?.as_array()?.iter().find_map(|a| {
                let url = a.get("browser_download_url")?.as_str()?;
                let name = a.get("name")?.as_str()?.to_ascii_lowercase();
                (name.ends_with(".zip") && !name.contains("sha256")).then(|| url.to_owned())
            })
        })
}

fn step_opti(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    if game::is_reshade_dll(&game::join_ci(d, &[game::RESHADE_PROXY])) {
        bail!(
            "ReShade is installed as dxgi.dll in this game; OptiScaler needs that name. \
             Run Remove (or Remove incl. ReShade) first, then install with the OptiScaler engine."
        );
    }
    progress(0, "Looking up latest OptiScaler DLSS-NR release");
    // An installed OptiScaler used to be left alone forever, so a game set up
    // in August still ran August's build after every reinstall. The tag is
    // recorded in the manifest; a copy this tool placed is refreshed when
    // upstream moves on, and one it did not place is never touched.
    let repo = opti_repo();
    let latest = net::latest_tag(client, repo).ok();
    if st.opti {
        // No manifest at all: somebody else put OptiScaler there. A manifest
        // without a "# tag" line is ours, from before the tag was recorded --
        // refresh it, which also writes the tag for next time.
        let Some(manifest) = fs::read_to_string(d.join(game::OPTI_MANIFEST)).ok() else {
            return Ok(vec![
                "OptiScaler present (not placed by this tool, left as is)".to_owned(),
            ]);
        };
        // A build from the other fork is replaced whatever its tag: the two
        // number their releases independently.
        let same_fork = manifest_repo(&manifest) == repo;
        match (manifest_tag(&manifest), &latest) {
            _ if !same_fork => progress(0, &format!("Switching OptiScaler to {repo}")),
            (Some(a), Some(b)) if &a == b => {
                return Ok(vec![format!("OptiScaler already current ({a})")]);
            }
            (Some(a), Some(b)) => progress(0, &format!("OptiScaler {a} is out, {b} available")),
            (Some(_), None) => {
                return Ok(vec![
                    "OptiScaler present (could not check for a newer one)".to_owned()
                ]);
            }
            (None, _) => progress(0, "OptiScaler version not recorded, refreshing"),
        }
    }
    // Stable release only (releases/latest skips pre-releases); the API list
    // and the releases page both put betas first.
    let asset: String = match latest.clone() {
        Some(tag) => net::github_asset_url_html(client, repo, &tag, r#"[^"]+\.zip"#)?,
        None => match net::get_json_github(client, &opti_releases_url()) {
            Ok(releases) => releases
                .as_array()
                .and_then(|a| pick_opti_zip(a))
                .ok_or_else(|| anyhow!("{repo} has no release asset"))?,
            Err(_) => {
                let tags = net::github_release_tags_html(client, repo, "v", 2)?;
                let tag = tags
                    .first()
                    .ok_or_else(|| anyhow!("no {repo} release found"))?;
                net::github_asset_url_html(client, repo, tag, r#"[^"]+\.zip"#)?
            }
        },
    };
    let asset = asset.as_str();
    let zip_path = work.join("optiscaler-dlssnr.zip");
    net::download(client, asset, &zip_path, "OptiScaler DLSS-NR", progress)?;

    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("OptiScaler download is not a valid zip")?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let mut installed: Vec<String> = Vec::new();
    for member in names {
        // This zip uses backslash separators; normalise, and never trust the path.
        let rel = member.replace('\\', "/");
        if rel.ends_with('/') {
            continue;
        }
        let parts: Vec<&str> = rel
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        if parts.is_empty() {
            continue;
        }
        let fname = parts.last().unwrap().to_string();
        // The interactive setup script and its banner file are not needed:
        // the renaming it performs is done right here.
        if fname.eq_ignore_ascii_case("setup_windows.bat")
            || fname.eq_ignore_ascii_case("setup_linux.sh")
            || fname.starts_with("!!")
        {
            continue;
        }
        let out_rel = if fname.eq_ignore_ascii_case("OptiScaler.dll") {
            game::RESHADE_PROXY.to_string() // dxgi.dll
        } else {
            parts.join("/")
        };
        let dest = d.join(out_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        // A refresh must not overwrite the settings file. It carries the user's
        // choices -- upscaler, frame generation, LoadReshade -- and replacing it
        // silently turns them all back to auto, which is how a working RenoDX
        // install stopped loading ReShade after a routine update.
        if fname.eq_ignore_ascii_case(OPTI_INI) && dest.is_file() {
            installed.push(out_rel);
            continue;
        }
        net::extract_member(&mut zip, &member, &dest)?;
        installed.push(out_rel);
    }
    if !installed.iter().any(|p| p == game::RESHADE_PROXY) {
        bail!("the OptiScaler release had no OptiScaler.dll — layout changed upstream");
    }
    // OptiScaler ships DLSS Neural Rendering off, and its overlay toggle lives
    // only in memory unless the user finds the Save button -- so the whole
    // point of this install had to be switched back on at every launch.
    let ini = d.join(OPTI_INI);
    if let Ok(text) = fs::read_to_string(&ini) {
        let mut cur = text;
        if let Some(patched) = set_dlss_nr_enabled(&cur) {
            cur = patched;
        }
        // RTX 40 multi-frame generation. This one is built into the fork and
        // memory-only — no file to fetch, nothing to sideload — so it is a
        // setting we can honestly turn on for someone. The Ampere/Turing
        // equivalent in the same ini sideloads a DLL that has no published
        // release, so it is deliberately not offered (#83).
        // Only the pre-SR build knows the key, and only where the gate lets
        // the unlock do anything; in Dagherbou's ini it would be a stray line.
        if install_extras().opti_presr {
            let value = if mfg_unavailable(st, Engine::Opti, true).is_none() {
                ada_mfg()
            } else {
                "false"
            };
            if let Some(patched) = set_ini_key(&cur, "FrameGen", "AdaMfgUnlock", value) {
                cur = patched;
            }
        }
        // RE Engine trips its own scheduler assertion unless the compute root
        // signature is put back, and fights REFramework over WndProc unless
        // input is polled. The graphics-side restores must stay off there: they
        // hand dangling descriptors to the NVIDIA driver when the swapchain is
        // recreated after the intro, which is a crash in nvwgf2umx.dll
        // (#44, Dragon's Dogma 2).
        if st.re_engine {
            for (key, value) in [
                ("ManualInputPolling", "true"),
                ("RestoreComputeSignature", "true"),
                ("RestoreGraphicSignature", "false"),
                ("ExtendedStateRestore", "false"),
            ] {
                if let Some(patched) = set_ini_key(&cur, "Hotfix", key, value) {
                    cur = patched;
                }
            }
        }
        fs::write(&ini, cur)?;
    }
    // The repo goes in beside the tag: the two builds number their releases
    // independently, so a tag alone cannot say whether v0.7.7 is current (#88).
    let header = latest
        .as_deref()
        .map(|t| format!("# tag {t}\n"))
        .unwrap_or_default();
    fs::write(
        d.join(game::OPTI_MANIFEST),
        format!("# repo {repo}\n{header}{}", installed.join("\n")),
    )?;
    installed.push(game::OPTI_MANIFEST.into());
    Ok(installed)
}

/// Take a Remix install back out: the model and marker from `.trex/`, the
/// neural-enable lines from rtx.conf, and any runtime we swapped (the mod's own
/// runtime is restored from the backup this tool kept). The mod itself is left
/// entirely alone.
fn uninstall_remix(trex: &Path) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for f in [game::DLSSNR_DLL, REMIX_MARKER] {
        let p = game::join_ci(trex, &[f]);
        if p.is_file() {
            fs::remove_file(&p)?;
            removed.push(format!(".trex/{f}"));
        }
    }
    let conf = crate::remix::conf_path(trex);
    if let Ok(text) = fs::read_to_string(&conf) {
        let mut t = text.clone();
        for key in ["rtx.neuralUplift.enable", "rtx.neuralRendering.enable"] {
            t = crate::remix::remove_option(&t, key);
        }
        if t != text {
            fs::write(&conf, &t)?;
            removed.push("rtx.conf neural-rendering enable".to_owned());
        }
    }
    for name in REMIX_RUNTIME_ASSETS {
        let dest = game::join_ci(trex, &[name]);
        let bak = dest.with_file_name(format!("{name}{REMIX_ORIG}"));
        if bak.is_file() {
            fs::copy(&bak, &dest)?;
            fs::remove_file(&bak)?;
            removed.push(format!(".trex/{name} (mod's own runtime restored)"));
        }
    }
    if removed.is_empty() {
        removed.push("no DLSS 5 Remix files to remove".to_owned());
    }
    Ok(removed)
}

/// Remove an OptiScaler install recorded in the manifest.
fn uninstall_opti(d: &Path, removed: &mut Vec<String>) -> Result<()> {
    let manifest = d.join(game::OPTI_MANIFEST);
    let Ok(list) = fs::read_to_string(&manifest) else {
        return Ok(());
    };
    for rel in list
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let clean: Vec<&str> = rel
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        let p = clean
            .iter()
            .fold(d.to_path_buf(), |acc, part| acc.join(part));
        if p.is_file() {
            fs::remove_file(&p)?;
            removed.push(rel.to_string());
        }
    }
    // Clean now-empty folders the archive created.
    for sub in ["OptiScaler/D3D12_OptiScaler", "OptiScaler", "Licenses"] {
        let p = d.join(sub.replace('/', std::path::MAIN_SEPARATOR_STR));
        if p.is_dir() && fs::read_dir(&p)?.next().is_none() {
            fs::remove_dir(&p)?;
        }
    }
    fs::remove_file(&manifest)?;
    removed.push(game::OPTI_MANIFEST.into());
    Ok(())
}

pub const BRIDGE_DOWNLOAD: &str =
    "https://github.com/NIGos/dlss5-bridge/releases/latest/download/dlss5-bridge.addon64";
/// matiasLombo/neural-upstream: the neural consumer that runs the network at the
/// game's render resolution instead of at output resolution, replacing the
/// RenoDX DLSS 5 add-on rather than joining it.
const UPSTREAM_DOWNLOAD: &str =
    "https://github.com/matiasLombo/neural-upstream/releases/latest/download/nvngx.dll.addon64";
/// lunks/dxvk-remix-plus-dlssnr: an RTX Remix runtime with the DLSS-NR stage
/// built in, a drop-in for a `.trex/` folder whose stock runtime has no neural
/// pass. Two loose assets, fetched by the plain "latest" redirect (no API).
const REMIX_RUNTIME_LATEST: &str =
    "https://github.com/lunks/dxvk-remix-plus-dlssnr/releases/latest/download/";
const REMIX_RUNTIME_ASSETS: [&str; 2] = ["d3d9.dll", "remix_nvngx.dll"];
/// Tag of the DLSS 5 model this tool placed inside a `.trex/`, for refresh.
const REMIX_MARKER: &str = ".dlss5oneclick-remix";
/// Suffix for a Remix runtime file we replaced, so a swap can be reverted.
const REMIX_ORIG: &str = ".dlss5oneclick-orig";
/// Records every file a downloaded RTX Remix *mod* placed, for exact removal.
const REMIX_MOD_MANIFEST: &str = ".dlss5oneclick-remix-mod";
/// Suffix for a game file the mod install replaced, so removal restores it.
const REMIX_MOD_ORIG: &str = ".dlss5oneclick-remix-orig";
/// mavismmg/MFGAdaUnlock-RenoDx: RTX 40 multi-frame generation as a ReShade
/// add-on. DangerousBerries reached 6X with this where OptiScaler's own
/// built-in unlock reported "DLSSG not patched: capability not matched" (#83).
const MFG_DOWNLOAD: &str = "https://github.com/mavismmg/MFGAdaUnlock-RenoDx/releases/latest/download/renodx-mfgunlock.addon64";
pub const RHI_RELEASES: &str =
    "https://api.github.com/repos/RankFTW/rhi-repo/releases?per_page=100";
pub const RHI_REPO: &str = "RankFTW/rhi-repo";
pub const OPTI_REPO: &str = "Dagherbou/OptiScaler_DLSSNR";
/// wilsjo2's fork: the neural pass runs before super resolution instead of
/// after it, with 1-3 configurable passes. Same zip layout as Dagherbou's, so
/// it installs through the same step (#72).
pub const OPTI_PRESR_REPO: &str = "wilsjo2/OptiScaler-DLSSNR-PreSR-Multipass";

/// Which OptiScaler build a scripted install asks for (`presr` = wilsjo2's
/// fork); unset means Dagherbou's. Read once by the CLI into
/// `Extras::opti_presr` -- nothing writes it at runtime.
pub const OPTI_SOURCE_ENV: &str = "DLSS5ONECLICK_OPTI_SOURCE";

/// True when `DLSS5ONECLICK_OPTI_SOURCE` asks for the pre-SR multipass fork.
pub fn opti_presr_from_env() -> bool {
    std::env::var(OPTI_SOURCE_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("presr"))
}

/// The OptiScaler repository the install running on this thread asked for.
pub fn opti_repo() -> &'static str {
    opti_repo_for(install_extras().opti_presr)
}

fn opti_repo_for(presr: bool) -> &'static str {
    if presr {
        OPTI_PRESR_REPO
    } else {
        OPTI_REPO
    }
}

fn opti_releases_url() -> String {
    format!("https://api.github.com/repos/{}/releases", opti_repo())
}

#[derive(Clone, Copy)]
pub struct Step {
    pub name: &'static str,
    pub run: fn(&Client, &GameStatus, &Path, Progress) -> Result<Vec<String>>,
}

const STEP_RESHADE: Step = Step {
    name: "ReShade (add-on build)",
    run: step_reshade,
};
const STEP_DGVOODOO: Step = Step {
    name: "dgVoodoo 2.87.3 (DX9 → D3D11)",
    run: step_dgvoodoo,
};
const STEP_HEADERS: Step = Step {
    name: "ReShade shader headers",
    run: step_headers,
};
const STEP_FEEDER: Step = Step {
    name: "DLSS5-Feeder",
    run: step_feeder,
};
const STEP_LUMENITE: Step = Step {
    name: "LumeniteFX motion vectors",
    run: step_lumenite,
};
const STEP_DLSS5: Step = Step {
    name: "DLSS 5 add-on + models",
    run: step_dlss5,
};
const STEP_DLSSNR_ONLY: Step = Step {
    name: "DLSS 5 model (nvngx_dlssnr.dll)",
    run: step_dlssnr_only,
};
const STEP_BRIDGE: Step = Step {
    name: "DLSS 5 DX11 bridge",
    run: step_bridge,
};
const STEP_REMIX: Step = Step {
    name: "DLSS 5 into the RTX Remix runtime",
    run: step_remix,
};
const STEP_REMIX_SWAP: Step = Step {
    name: "Swap in a DLSS 5-capable Remix runtime",
    run: step_remix_swap,
};
const STEP_MFG: Step = Step {
    name: "RTX 40 multi-frame generation add-on",
    run: step_mfg,
};
const STEP_UPSTREAM: Step = Step {
    name: "Neural Upstream add-on (experimental)",
    run: step_upstream,
};
const STEP_CONFIG: Step = Step {
    name: "ReShade config",
    run: step_config,
};
const STEP_FEEDER_CLEANUP: Step = Step {
    name: "Remove DLSS5-Feeder (game has native DLSS)",
    run: step_feeder_cleanup,
};
const STEP_DLSS5_CLEANUP: Step = Step {
    name: "Remove the RenoDX DLSS 5 add-on (Neural Upstream replaces it)",
    run: step_dlss5_cleanup,
};
const STEP_REFRAMEWORK: Step = Step {
    name: "REFramework (RE Engine needs it before ReShade)",
    run: step_reframework,
};
const STEP_RENODX: Step = Step {
    name: "RenoDX HDR mod for this game",
    run: step_renodx,
};
const STEP_HOST_RESHADE: Step = Step {
    name: "64-bit ReShade for the host64 helper",
    run: step_host_reshade,
};

/// 32-bit games: the helper process needs its own 64-bit ReShade as
/// `host64\dxgi.dll` (the Feeder README: "run the ReShade installer once
/// against any 64-bit game and take it from there"). Same marker/refresh rule
/// as the in-game copy.
fn step_host_reshade(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let host = st.consumer_dir();
    fs::create_dir_all(&host)?;
    progress(0, "Looking up latest ReShade");
    let (ver, url) = resolve_reshade_setup(client)?;
    if st.host_reshade {
        match fs::read_to_string(host.join(game::RESHADE_MARKER)) {
            Ok(mine) if mine.trim() == ver => {
                return Ok(vec![format!("host64/dxgi.dll already current ({ver})")]);
            }
            Ok(_) => progress(0, &format!("host64 ReShade {ver} is out, refreshing")),
            Err(_) => {
                return Ok(vec![
                    "host64/dxgi.dll present (not placed by this tool)".into()
                ])
            }
        }
    }
    let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
    net::download(client, &url, &setup, "ReShade (64-bit, host64)", progress)?;
    install_reshade_from_setup(&setup, &host, 64, game::RESHADE_PROXY)?;
    fs::write(host.join(game::RESHADE_MARKER), ver.as_bytes())?;
    Ok(vec![format!("{}/{}", game::HOST_DIR, game::RESHADE_PROXY)])
}

const STEP_GPU_PREF: Step = Step {
    name: "GPU preference",
    run: step_gpu_pref,
};
const STEP_RESHADE_VIA_OPTI: Step = Step {
    name: "ReShade loaded by OptiScaler (ReShade64.dll)",
    run: step_reshade_via_opti,
};

/// ReShade beside OptiScaler, the way OptiScaler.ini documents it: the ReShade
/// DLL as `ReShade64.dll` next to the exe and `[Plugins] LoadReshade=true`, so
/// OptiScaler (which holds dxgi.dll) loads it and ReShade add-ons still work.
fn step_reshade_via_opti(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let ini = d.join(OPTI_INI);
    if !ini.is_file() {
        bail!("{OPTI_INI} not found — install the OptiScaler engine first");
    }
    let mut done = Vec::new();
    let dll = d.join(RESHADE64);
    if !dll.is_file() {
        progress(0, "Looking up latest ReShade");
        let (ver, url) = resolve_reshade_setup(client)?;
        let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
        net::download(client, &url, &setup, "ReShade", progress)?;
        install_reshade_from_setup(&setup, d, st.bitness, RESHADE64)?;
        // Recorded in the OptiScaler manifest so Remove takes it out with the engine.
        let mut m = fs::read_to_string(d.join(game::OPTI_MANIFEST)).unwrap_or_default();
        if !m.lines().any(|l| l == RESHADE64) {
            if !m.is_empty() && !m.ends_with('\n') {
                m.push('\n');
            }
            m.push_str(RESHADE64);
            fs::write(d.join(game::OPTI_MANIFEST), m)?;
        }
        done.push(RESHADE64.to_owned());
    }
    let text = fs::read_to_string(&ini)?;
    if let Some(new) = set_load_reshade(&text) {
        fs::write(&ini, new)?;
        done.push(format!("{OPTI_INI}: LoadReshade=true"));
    }
    if done.is_empty() {
        progress(100, "ReShade64.dll + LoadReshade already set");
    }
    Ok(done)
}

pub const OPTI_INI: &str = "OptiScaler.ini";
pub const RESHADE64: &str = "ReShade64.dll";

/// `LoadReshade=true` in OptiScaler.ini; `None` when already set.
pub fn set_load_reshade(ini: &str) -> Option<String> {
    let mut out = String::with_capacity(ini.len() + 32);
    let mut seen = false;
    let mut changed = false;
    for line in ini.split_inclusive('\n') {
        let t = line.trim_end_matches(['\r', '\n']);
        let key = t.split('=').next().unwrap_or("").trim();
        if key.eq_ignore_ascii_case("LoadReshade") {
            seen = true;
            if t.split('=').nth(1).map(str::trim) != Some("true") {
                out.push_str("LoadReshade=true");
                out.push_str(&line[t.len()..]);
                changed = true;
                continue;
            }
        }
        out.push_str(line);
    }
    if !seen {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[Plugins]\nLoadReshade=true\n");
        changed = true;
    }
    changed.then_some(out)
}

/// `[DlssNr] Enabled=true` in OptiScaler.ini; `None` when it already says so.
/// Section-scoped: `Enabled` appears under half a dozen headings in that file.
pub fn set_dlss_nr_enabled(ini: &str) -> Option<String> {
    set_ini_key(ini, "DlssNr", "Enabled", "true")
}

/// The two FSR 3.1 frame-generation libraries OptiScaler bundles. `FGOutput=fsrfg`
/// needs both beside the DLL (under `OptiScaler/`); a trimmed build without them
/// cannot frame-generate, so the toggle is a no-op there rather than a wrong ini.
const FG_LIBS: [&str; 2] = [
    "amd_fidelityfx_loader_dx12.dll",
    "amd_fidelityfx_framegeneration_dx12.dll",
];

/// Turn on OptiScaler's FSR 3.1 frame generation, driven by the upscaler it
/// already runs — 2× on any RTX card, D3D12 only. Separate from the RTX 40 MFG
/// unlock (which multiplies a DLSS Frame Generation the game already has). The
/// keys and values are verified against a real OptiScaler.ini: `[FrameGen]`
/// Enabled/FGInput=upscaler/FGOutput=fsrfg, and `[OptiFG] HUDFix=true` (the
/// upscaler input needs Hudfix or the UI ghosts). Experimental under Proton.
fn step_opti_fg(
    _client: &Client,
    st: &GameStatus,
    _work: &Path,
    _progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let ini = d.join(OPTI_INI);
    let Ok(text) = fs::read_to_string(&ini) else {
        return Ok(vec![
            "frame generation skipped (OptiScaler.ini not found)".to_owned(),
        ]);
    };
    let opti_sub = game::join_ci(d, &["OptiScaler"]);
    let libs_present = FG_LIBS
        .iter()
        .all(|l| game::join_ci(&opti_sub, &[l]).is_file());
    if !libs_present {
        return Ok(vec![
            "frame generation not available in this OptiScaler build (FSR 3.1 libraries absent)"
                .to_owned(),
        ]);
    }
    let (cur, set) = fg_ini(&text);
    if set.is_empty() {
        return Ok(vec!["frame generation already on".to_owned()]);
    }
    fs::write(&ini, cur)?;
    Ok(vec![format!(
        "frame generation on ({}) — turn the game's own frame generation off; \
         experimental under Proton",
        set.join(", ")
    )])
}

/// Set `[DlssNr] WorkingScale` — the fraction of native the neural model runs
/// at — in an OptiScaler.ini. Two decimals, section-scoped. `None` when it
/// already reads that way.
fn scale_ini(ini: &str, scale: f32) -> Option<String> {
    set_ini_key(ini, "DlssNr", "WorkingScale", &format!("{scale:.2}"))
}

/// The model resolution this game's OptiScaler.ini already asks for, so the
/// GUI dial opens on it and a reinstall does not quietly reset hand tuning.
/// `None` without an ini, or when the key is absent, `auto` or out of range.
pub fn opti_working_scale(game_dir: &Path) -> Option<f32> {
    let text = fs::read_to_string(game::join_ci(game_dir, &[OPTI_INI])).ok()?;
    get_ini_key(&text, "DlssNr", "WorkingScale")?
        .parse::<f32>()
        .ok()
        .filter(|f| (0.25..=2.0).contains(f))
}

/// Whether this game's OptiScaler.ini already has the pre-SR build's RTX 40
/// MFG unlock on, so the tick opens on it and a reinstall keeps it.
pub fn opti_ada_mfg(game_dir: &Path) -> bool {
    fs::read_to_string(game::join_ci(game_dir, &[OPTI_INI]))
        .ok()
        .and_then(|t| {
            get_ini_key(&t, "FrameGen", "AdaMfgUnlock").map(|v| v.eq_ignore_ascii_case("true"))
        })
        .unwrap_or(false)
}

/// `key`'s value in `[section]`, matched the way `set_ini_key` matches it.
fn get_ini_key<'a>(ini: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in ini.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t.eq_ignore_ascii_case(&header);
        } else if in_section {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == key {
                    return Some(v.trim());
                }
            }
        }
    }
    None
}

/// Apply the FSR 3.1 frame-generation keys to an OptiScaler.ini, section-scoped
/// so no unrelated `Enabled` moves. Returns the patched text and which keys
/// changed (empty when it already reads that way — the step is then a no-op).
fn fg_ini(ini: &str) -> (String, Vec<String>) {
    let mut cur = ini.to_string();
    let mut set = Vec::new();
    for (section, key, value) in [
        ("FrameGen", "Enabled", "true"),
        ("FrameGen", "FGInput", "upscaler"),
        ("FrameGen", "FGOutput", "fsrfg"),
        ("OptiFG", "HUDFix", "true"),
    ] {
        if let Some(patched) = set_ini_key(&cur, section, key, value) {
            cur = patched;
            set.push(format!("[{section}] {key}={value}"));
        }
    }
    (cur, set)
}

/// Set `key=value` inside `[section]`, appending the section or the key when
/// missing; `None` when it already reads that way. Section-scoped because
/// OptiScaler.ini repeats names like `Enabled` under many headings.
pub fn set_ini_key(ini: &str, section: &str, key: &str, value: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut out = String::with_capacity(ini.len() + 32);
    let mut in_section = false;
    let mut seen = false;
    let mut changed = false;
    for line in ini.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\r', '\n']);
        let t = raw.trim();
        if t.starts_with('[') {
            in_section = t.eq_ignore_ascii_case(&header);
        } else if in_section && t.split('=').next().unwrap_or("").trim() == key {
            seen = true;
            if t.split('=').nth(1).map(str::trim) != Some(value) {
                out.push_str(&format!("{key}={value}"));
                out.push_str(&line[raw.len()..]);
                changed = true;
                continue;
            }
        }
        out.push_str(line);
    }
    if !seen {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("\n{header}\n{key}={value}\n"));
        changed = true;
    }
    changed.then_some(out)
}

pub const REFRAMEWORK_ZIP: &str =
    "https://github.com/praydog/REFramework-nightly/releases/latest/download/REFramework.zip";

/// praydog's monolithic nightly: one `dinput8.dll` that detects the RE Engine
/// game at runtime (DMC5, RE2/3/4/7/8/9, MHRise, MHWilds, SF6, DD2, Pragmata...).
/// Only the DLL is extracted, as its release notes insist.
fn step_reframework(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if st.reframework {
        progress(100, "REFramework already present");
        return Ok(vec![]);
    }
    let d = st.game_dir();
    let zip_path = work.join("REFramework.zip");
    net::download(client, REFRAMEWORK_ZIP, &zip_path, "REFramework", progress)?;
    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("REFramework download is not a valid zip")?;
    let member = zip
        .file_names()
        .find(|n| net::file_name(n).eq_ignore_ascii_case(game::REFRAMEWORK_DLL))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("REFramework.zip has no {}", game::REFRAMEWORK_DLL))?;
    net::extract_member(&mut zip, &member, &d.join(game::REFRAMEWORK_DLL))?;
    fs::write(d.join(game::REFRAMEWORK_MARKER), b"")?;
    Ok(vec![game::REFRAMEWORK_DLL.to_owned()])
}

fn step_renodx(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    progress(0, "Looking up the RenoDX mod for this game");
    let m = renodx::lookup(client, &st.exe)?
        .ok_or_else(|| anyhow!("no RenoDX mod is published for this game"))?;
    renodx::install(client, &st.exe, &m, progress)
}

const STEP_MFG_ASI_CLEANUP: Step = Step {
    name: "Take out the older RTX 40 MFG unlock",
    run: step_mfg_asi_cleanup,
};

/// dashdogy's unlock, which this fork shipped before adopting upstream's, must
/// never run beside it: both patch frame generation in memory. Its manifest
/// says exactly what to take out.
fn step_mfg_asi_cleanup(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    progress(0, "Taking out the older RTX 40 MFG unlock");
    let mut removed = Vec::new();
    crate::mfg::uninstall(st.game_dir(), &mut removed)?;
    Ok(removed)
}

/// Why RTX 40 multi-frame generation cannot be offered for this game on this
/// route, or `None` when it can. The GUI shows it beside a disabled tick,
/// `--check` prints it, and the plan will not place the unlock without it.
pub fn mfg_unavailable(st: &GameStatus, engine: Engine, opti_presr: bool) -> Option<&'static str> {
    use crate::gpu::Tier;
    if st.remix.is_some() {
        return Some("not on the RTX Remix route");
    }
    match st.gpu.as_ref().map(|(_, t)| *t) {
        Some(Tier::Rtx40) => {}
        Some(Tier::Rtx50) => return Some("the RTX 50 series has multi-frame generation of its own"),
        _ => return Some("it unlocks RTX 40 cards only"),
    }
    if !st.has_fg {
        return Some("this game has no DLSS Frame Generation of its own to multiply");
    }
    if engine == Engine::Opti {
        if !opti_presr {
            return Some("on OptiScaler it is built into the experimental pre-SR build only");
        }
    } else if st.is32() {
        return Some("the add-on is 64-bit and this is a 32-bit game");
    }
    None
}

/// `with_renodx` adds the game's RenoDX HDR mod after the DLSS 5 add-on. On
/// the OptiScaler engine that needs ReShade too, loaded by OptiScaler as
/// `ReShade64.dll`. RE Engine games get REFramework first on either engine.
/// The optional extras a caller turns on for an install. Bundled into one value
/// so the two entry points do not grow a positional bool per feature — a shape
/// that has already been mismerged once (each `false` looked like every other).
#[derive(Clone, Copy, Default)]
pub struct Extras {
    /// Also install the game's RenoDX HDR mod after the DLSS 5 add-on.
    pub with_renodx: bool,
    /// ReShade engine: run the experimental Neural Upstream consumer.
    pub upstream: bool,
    /// OptiScaler engine: turn on FSR 3.1 frame generation (any RTX card, D3D12).
    pub with_fg: bool,
    /// OptiScaler engine: the fraction of native the neural model runs at
    /// (`[DlssNr] WorkingScale`; cost falls with its square). `None` leaves the
    /// ini's own value; the GUI passes its dial, which opens on that value.
    pub model_scale: Option<f32>,
    /// Remix route: replace a runtime that has no neural pass with a DLSS
    /// 5-capable community one (originals backed up; experimental).
    pub remix_swap: bool,
    /// OptiScaler engine: install wilsjo2's pre-SR multipass fork instead of
    /// Dagherbou's build (#72).
    pub opti_presr: bool,
    /// ReShade route: hold the DLSS 5 add-on on the classic build (#69).
    pub classic_addon: bool,
    /// Neural Upstream: strength preset to seed into ReShade.ini; 0 leaves the
    /// overlay's own (`reshade_ini::UPSTREAM_PRESETS`).
    pub upstream_preset: u8,
    /// RTX 40 multi-frame generation (#83): the pre-SR OptiScaler build's own
    /// `AdaMfgUnlock` on that route, mavismmg's add-on on the ReShade route.
    pub ada_mfg: bool,
}

pub fn plan_with(st: &GameStatus, engine: Engine, x: Extras) -> Vec<Step> {
    // A Remix game takes the Remix route regardless of engine: the neural pass
    // lives inside the mod's runtime, so there is no ReShade/OptiScaler here.
    if st.remix.is_some() {
        let mut v = Vec::new();
        if x.remix_swap {
            v.push(STEP_REMIX_SWAP); // swap the runtime first, then feed the new one
        }
        v.push(STEP_REMIX);
        return v;
    }
    let mut v = if engine == Engine::Opti {
        // Only games with native DLSS: the NR pass reads the inputs the game
        // hands to DLSS. Callers gate on mode; return the plan regardless so
        // --check can show it.
        let mut v = vec![STEP_OPTI];
        if x.with_fg {
            v.push(STEP_OPTI_FG); // edits the ini step_opti just wrote
        }
        v.push(STEP_DLSSNR_ONLY);
        if x.with_renodx {
            v.push(STEP_RESHADE_VIA_OPTI);
            v.push(STEP_RENODX);
        }
        v
    } else {
        let mut v = plan_reshade(st, x.upstream);
        if x.with_renodx {
            let at = v.len() - 1; // before ReShade config
            v.insert(at, STEP_RENODX);
        }
        v
    };
    // RTX 40 multi-frame generation. On the OptiScaler route the pre-SR build
    // writes its own ini key (step_opti); on the ReShade route it is this
    // separate add-on, which is what actually reached 6X for the reporter in
    // #83. The older dashdogy unlock comes out first on either route: the two
    // patch the same thing in memory and must never run together.
    if x.ada_mfg && mfg_unavailable(st, engine, x.opti_presr).is_none() {
        if engine != Engine::Opti {
            let at = v.len().saturating_sub(1); // before ReShade config
            v.insert(at, STEP_MFG);
        }
        if st.mfg_asi {
            v.insert(0, STEP_MFG_ASI_CLEANUP);
        }
    }
    if st.re_engine {
        v.insert(0, STEP_REFRAMEWORK);
    }
    // DX9 never loads dxgi.dll; dgVoodoo must sit in the game folder first.
    // Always run on Dx9 (even when the DLL is already present) so Install can
    // refresh dgVoodoo.conf — Uninstall never removes dgVoodoo, and a bare
    // OutputAPI-only conf leaves stock VRAM=256 (Gothic 3 texture failures).
    if st.api == game::Api::Dx9 {
        v.insert(0, STEP_DGVOODOO);
    }
    v.push(STEP_GPU_PREF);
    v
}

fn plan_reshade(st: &GameStatus, upstream: bool) -> Vec<Step> {
    match st.mode {
        game::Mode::Feeder => {
            let mut v = vec![STEP_RESHADE];
            if st.is32() {
                v.push(STEP_HOST_RESHADE);
            }
            v.extend([
                STEP_HEADERS,
                STEP_FEEDER,
                STEP_LUMENITE,
                STEP_DLSS5,
                STEP_CONFIG,
            ]);
            v
        }
        game::Mode::Native => {
            let mut v = vec![STEP_RESHADE];
            if st.feeder {
                v.push(STEP_FEEDER_CLEANUP);
            }
            // Neural Upstream is itself the neural consumer: it creates the
            // DLSSNR feature and needs only the model beside it, so it takes
            // the RenoDX add-on's place rather than sitting next to it.
            if upstream {
                v.push(STEP_DLSS5_CLEANUP);
                v.push(STEP_UPSTREAM);
                v.push(STEP_DLSSNR_ONLY);
            } else {
                v.push(STEP_DLSS5);
            }
            if st.needs_bridge() {
                v.push(STEP_BRIDGE);
            }
            v.push(STEP_CONFIG);
            v
        }
    }
}

// ── release picking ────────────────────────────────────────────────

fn ver_key(tag: &str, prefix: &str) -> Vec<u64> {
    Regex::new(r"\d+")
        .unwrap()
        .find_iter(&tag[prefix.len()..])
        .filter_map(|m| m.as_str().parse().ok())
        .collect()
}

/// Newest rhi-repo release whose tag is `prefix` + digits; returns (tag, first asset URL).
pub fn pick_latest_asset(releases: &[Value], prefix: &str) -> Result<(String, String)> {
    let cands: Vec<(Vec<u64>, String, String)> = releases
        .iter()
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let rest = tag.strip_prefix(prefix)?;
            if !rest.chars().next()?.is_ascii_digit() {
                return None; // "dlss-" must not match "dlssg-"
            }
            let url = r
                .get("assets")?
                .as_array()?
                .first()?
                .get("browser_download_url")?
                .as_str()?;
            Some((ver_key(tag, prefix), tag.to_owned(), url.to_owned()))
        })
        .collect();
    if cands.is_empty() {
        bail!("no release with tag prefix '{prefix}' found");
    }
    Ok(best_tag(cands))
}

/// Newest by version; for the DLSS 5 model prefer ShortFuse's multi-generation
/// `.SF` builds over NVIDIA's RTX-50-only originals or single-generation ports.
fn best_tag(mut cands: Vec<(Vec<u64>, String, String)>) -> (String, String) {
    let any_sf = cands
        .iter()
        .any(|(_, t, _)| t.starts_with("dlssnr-") && t.contains(".SF"));
    if any_sf {
        cands.retain(|(_, t, _)| t.contains(".SF"));
    }
    cands.sort();
    let (_, tag, url) = cands.pop().unwrap();
    (tag, url)
}

/// Tag of a DLSS 5 add-on build to install outright (`renodx-dlss5-4.6`);
/// unset means this tool chooses. Only ever read: the GUI's classic tick
/// travels in `Extras::classic_addon` instead of being written here.
pub const RENODX_TAG_ENV: &str = "DLSS5ONECLICK_RENODX_TAG";

/// The classic-engine add-on. The Feeder's own host measured v4.7 to fault
/// inside the driver's NGX runtime on NVIDIA 616.64 — an access violation in
/// D3D12Core.dll reached through nvngx_dlssnr.dll — and names this build as one
/// that passes there (#69).
pub const RENODX_CLASSIC_TAG: &str = "renodx-dlss5-4.55";

/// `RENODX_CLASSIC_TAG`'s line as `rhi_pinned` matches it: 4.55 and any 4.55.x.
const CLASSIC_LINE: &str = "4.55";

/// A pinned add-on build, when one was asked for: `(tag, url)`.
fn rhi_env_pinned(client: &Client, prefix: &str) -> Option<Result<(String, String)>> {
    if prefix != "renodx-dlss5-" {
        return None;
    }
    let tag = std::env::var(RENODX_TAG_ENV).ok()?;
    if tag.is_empty() {
        return None;
    }
    Some(
        net::github_asset_url_html(client, RHI_REPO, &tag, r#"[^"]+\.zip"#)
            .map(|url| (tag.clone(), url))
            .with_context(|| format!("DLSS 5 add-on build {tag} not found on {RHI_REPO}")),
    )
}

/// The newest rhi-repo release for `prefix`: `(tag, url)`. Tries the API, then
/// the HTML releases pages for the tag and the expanded-assets fragment for
/// the file, so it never needs the API.
pub fn rhi_latest(client: &Client, prefix: &str) -> Result<(String, String)> {
    if let Some(pinned) = rhi_env_pinned(client, prefix) {
        return pinned;
    }
    if let Ok(releases) = net::get_json_github(client, RHI_RELEASES) {
        if let Some(arr) = releases.as_array() {
            if let Ok(r) = pick_latest_asset(arr, prefix) {
                return Ok(r);
            }
        }
    }
    let tags = net::github_release_tags_html(client, RHI_REPO, prefix, 6)?;
    let cands: Vec<(Vec<u64>, String, String)> = tags
        .into_iter()
        .filter(|t| {
            t[prefix.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .map(|t| (ver_key(&t, prefix), t, String::new()))
        .collect();
    if cands.is_empty() {
        bail!("no release with tag prefix '{prefix}' found on github.com/{RHI_REPO}/releases");
    }
    let (tag, _) = best_tag(cands);
    let url = net::github_asset_url_html(client, RHI_REPO, &tag, r#"[^"]+\.zip"#)?;
    Ok((tag, url))
}

// ── add-on pinning (feeder ↔ renodx-dlss5) ─────────────────────────
//
// The DLSS5-Feeder and the renodx-dlss5 add-on are one contract: a feeder
// release supports only certain add-on generations, and pairing a newer add-on
// with an older feeder is the `CreateFeature 0xC0000005` crash on an otherwise
// correct install. jlrouzies-fr pins it in the feeder's own README — the stable
// line (< 0.8.0-beta.3) works only with 4.55; 0.8.0-beta.3 added 4.6 and
// 0.9.0-beta.1 added 4.7. The driver side of the same fault (NVIDIA's DLSS 5
// launch drivers route NGX feature 18 into the runtime, where 4.6/4.7 fault on
// every evaluate) is no longer guessed from a version threshold: the Feeder's
// host measures it on this machine and says so in its log, and
// `addon_faulted_in_driver` keeps that verdict per driver (#69). Either way
// 4.55 is the known-good build, so holding to it is the safe direction.

/// 'v0.9.0-beta.1' -> [0,9,0,0,1]: sortable, a beta sorting below its release.
fn feeder_key(tag: &str) -> Vec<u64> {
    let nums: Vec<u64> = tag
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| (!s.is_empty()).then(|| s.parse().ok()).flatten())
        .collect();
    let mut base: Vec<u64> = nums.iter().take(3).copied().collect();
    base.resize(3, 0);
    if tag.to_ascii_lowercase().contains("beta") {
        base.push(0);
        base.push(nums.get(3).copied().unwrap_or(0));
    } else {
        base.push(1);
        base.push(0);
    }
    base
}

/// True when the feeder being installed is too old for any add-on newer than
/// the classic build. The native route has no feeder, so no constraint.
fn feeder_needs_classic(feeder_tag: Option<&str>) -> bool {
    feeder_tag.is_some_and(|ft| feeder_key(ft) < feeder_key("v0.8.0-beta.3"))
}

/// The label part of an rhi-repo tag (`renodx-dlss5-4.55` -> `4.55`) equals or
/// sits on the same dotted line as `want` (`4.55` matches `4.55` and `4.55.1`).
fn label_is(tag: &str, prefix: &str, want: &str) -> bool {
    match tag.strip_prefix(prefix) {
        Some(rest) => rest == want || rest.starts_with(&format!("{want}.")),
        None => false,
    }
}

/// Like `rhi_latest`, but when `pin` is set it selects the newest release on
/// that pinned line instead of the newest overall. Falls back to the newest if
/// the pinned build cannot be found, so a pin can never make a route unavailable.
pub fn rhi_pinned(client: &Client, prefix: &str, pin: Option<&str>) -> Result<(String, String)> {
    let Some(pin) = pin else {
        return rhi_latest(client, prefix);
    };
    if let Ok(releases) = net::get_json_github(client, RHI_RELEASES) {
        if let Some(arr) = releases.as_array() {
            let cands: Vec<(Vec<u64>, String, String)> = arr
                .iter()
                .filter_map(|r| {
                    let tag = r.get("tag_name")?.as_str()?;
                    if !label_is(tag, prefix, pin) {
                        return None;
                    }
                    let url = r
                        .get("assets")?
                        .as_array()?
                        .first()?
                        .get("browser_download_url")?
                        .as_str()?;
                    Some((ver_key(tag, prefix), tag.to_owned(), url.to_owned()))
                })
                .collect();
            if !cands.is_empty() {
                return Ok(best_tag(cands));
            }
        }
    }
    let tags = net::github_release_tags_html(client, RHI_REPO, prefix, 20)?;
    if let Some(tag) = tags.into_iter().find(|t| label_is(t, prefix, pin)) {
        let url = net::github_asset_url_html(client, RHI_REPO, &tag, r#"[^"]+\.zip"#)?;
        return Ok((tag, url));
    }
    // The pinned build is not on GitHub any more: newest is better than nothing.
    rhi_latest(client, prefix)
}

/// Refuse a downloaded `nvngx_dlssnr.dll` that has no code for this card, before
/// the install can look finished while nothing renders. Only refuses when the
/// card's exact `sm` is known (nvidia-smi) and absent from the file's fatbins;
/// silent otherwise, and skipped with the GPU check (the GUI tick or
/// `DLSS5ONECLICK_SKIP_GPU_CHECK`).
fn validate_dlssnr(dll: &Path) -> Result<()> {
    if game::skip_gpu_check() {
        return Ok(());
    }
    let Some(sm) = gpu::compute_capability() else {
        return Ok(());
    };
    let archs = gpu::dll_architectures(dll);
    if archs.is_empty() || archs.contains(&sm) {
        return Ok(());
    }
    let have = archs
        .iter()
        .map(|a| format!("sm_{a}"))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "This DLSS 5 model has no code for your card. It was built for {have}, and your card is \
         sm_{sm} — it would load and never produce a neural frame (every log would still say \
         success). Pick a different DLSS 5 model build, or set DLSS5ONECLICK_SKIP_GPU_CHECK=1 to \
         install it anyway."
    );
}

// ── step 1: ReShade ────────────────────────────────────────────────

pub fn resolve_reshade_setup(client: &Client) -> Result<(String, String)> {
    let html = net::get_text(client, RESHADE_HOME)?;
    let re = Regex::new(r"/downloads/ReShade_Setup_([\d.]+)_Addon\.exe").unwrap();
    let m = re
        .captures(&html)
        .ok_or_else(|| anyhow!("ReShade add-on installer link not found on reshade.me"))?;
    Ok((m[1].to_owned(), format!("{RESHADE_HOME}{}", &m[0])))
}

pub fn install_reshade_from_setup(
    setup_exe: &Path,
    game_dir: &Path,
    bitness: u8,
    dest_name: &str,
) -> Result<Vec<String>> {
    let dll = if bitness == 64 {
        "ReShade64.dll"
    } else {
        "ReShade32.dll"
    };
    let f = fs::File::open(setup_exe)?;
    let mut zip = zip::ZipArchive::new(f).context("ReShade installer has no readable archive")?;
    net::extract_member(&mut zip, dll, &game_dir.join(dest_name))
        .with_context(|| format!("{} does not contain {dll}", setup_exe.display()))?;
    Ok(vec![dest_name.into()])
}

/// Parse a loose dgVoodoo-style INI and ensure Feeder-safe keys without wiping CPL settings.
/// - Force `OutputAPI = d3d11_fl11_0` under `[General]`
/// - Floor `VRAM` under `[DirectX]` to at least [`DGVOODOO_VRAM_FLOOR`]
/// - Create missing sections/keys; leave every other line untouched
fn merge_dgvoodoo_conf(existing: &str) -> String {
    let mut out = String::with_capacity(existing.len() + 128);
    let mut section = String::new();
    let mut saw_general = false;
    let mut saw_directx = false;
    let mut output_api_set = false;
    let mut vram_set = false;
    let mut watermark_set = false;

    for raw in existing.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
            // Flush required keys before leaving a section.
            if section.eq_ignore_ascii_case("General") && !output_api_set {
                out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
                output_api_set = true;
            }
            if section.eq_ignore_ascii_case("DirectX") {
                if !vram_set {
                    out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
                    vram_set = true;
                }
                if !watermark_set {
                    out.push_str("dgVoodooWatermark = false\n");
                    watermark_set = true;
                }
            }
            section = trimmed[1..trimmed.len() - 1].to_string();
            if section.eq_ignore_ascii_case("General") {
                saw_general = true;
            }
            if section.eq_ignore_ascii_case("DirectX") {
                saw_directx = true;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if section.eq_ignore_ascii_case("General") && key.eq_ignore_ascii_case("OutputAPI") {
                out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
                output_api_set = true;
                continue;
            }
            if section.eq_ignore_ascii_case("DirectX") && key.eq_ignore_ascii_case("VRAM") {
                let cur = val
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let floor = cur.max(DGVOODOO_VRAM_FLOOR);
                out.push_str(&format!("VRAM = {floor}\n"));
                vram_set = true;
                continue;
            }
            if section.eq_ignore_ascii_case("DirectX")
                && key.eq_ignore_ascii_case("dgVoodooWatermark")
            {
                out.push_str("dgVoodooWatermark = false\n");
                watermark_set = true;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if section.eq_ignore_ascii_case("General") && !output_api_set {
        out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
        output_api_set = true;
    }
    if section.eq_ignore_ascii_case("DirectX") {
        if !vram_set {
            out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
            vram_set = true;
        }
        if !watermark_set {
            out.push_str("dgVoodooWatermark = false\n");
            watermark_set = true;
        }
    }

    if !saw_general {
        out.push_str("\n[General]\n");
        out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
        output_api_set = true;
    } else if !output_api_set {
        // Section existed but key never appeared (empty section mid-file already handled).
        out.push_str(&format!("OutputAPI = {DGVOODOO_OUTPUT_API}\n"));
    }

    if !saw_directx {
        out.push_str("\n[DirectX]\n");
        out.push_str("VideoCard = geforce_9800_gt\n");
        out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
        out.push_str("dgVoodooWatermark = false\n");
        out.push_str("Antialiasing = appdriven\n");
        out.push_str("FastVideoMemoryAccess = false\n");
    } else {
        if !vram_set {
            out.push_str(&format!("VRAM = {DGVOODOO_VRAM_FLOOR}\n"));
        }
        if !watermark_set {
            out.push_str("dgVoodooWatermark = false\n");
        }
    }

    let _ = (output_api_set, vram_set);
    out
}

fn assert_dgvoodoo_conf_healthy(text: &str) -> Result<()> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("outputapi") || !lower.contains("d3d11_fl11_0") {
        bail!("dgVoodoo.conf health check failed: OutputAPI must be d3d11_fl11_0");
    }
    // Find VRAM value
    let mut vram_ok = false;
    let mut section = "";
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            section = t;
            continue;
        }
        if section.eq_ignore_ascii_case("[DirectX]") {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case("VRAM") {
                    let n = v
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);
                    vram_ok = n >= DGVOODOO_VRAM_FLOOR;
                }
            }
        }
    }
    if !vram_ok {
        bail!("dgVoodoo.conf health check failed: VRAM must be >= {DGVOODOO_VRAM_FLOOR}");
    }
    Ok(())
}

/// Smart-merge (or create) `dgVoodoo.conf`: force OutputAPI, floor VRAM, preserve the rest.
/// Writes `dgVoodoo.conf.bak` once before the first edit of an existing file.
pub fn write_dgvoodoo_conf(game_dir: &Path) -> Result<()> {
    let conf = game::join_ci(game_dir, &["dgVoodoo.conf"]);
    let bak = game_dir.join("dgVoodoo.conf.bak");
    let text = if conf.is_file() {
        let existing =
            fs::read_to_string(&conf).with_context(|| format!("reading {}", conf.display()))?;
        if !bak.is_file() {
            fs::write(&bak, &existing).with_context(|| format!("writing {}", bak.display()))?;
        }
        merge_dgvoodoo_conf(&existing)
    } else {
        DGVOODOO_CONF_TEMPLATE.to_string()
    };
    assert_dgvoodoo_conf_healthy(&text)?;
    fs::write(&conf, text).with_context(|| format!("writing {}", conf.display()))?;
    Ok(())
}

fn dgvoodoo_d3d9_member(bitness: u8) -> &'static str {
    if bitness == 64 {
        DGVOODOO_D3D9_MEMBER_X64
    } else {
        DGVOODOO_D3D9_MEMBER_X86
    }
}

/// Place official dgVoodoo `MS/{x86|x64}/D3D9.dll` + smart-merged conf in the game folder.
/// Never restores `d3d9.dll.off` (old ReShade); always extracts from the release zip.
pub fn install_dgvoodoo_from_zip(
    zip_path: &Path,
    game_dir: &Path,
    bitness: u8,
) -> Result<Vec<String>> {
    let want = dgvoodoo_d3d9_member(bitness);
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("dgVoodoo download is not a valid zip")?;
    let member = zip
        .file_names()
        .find(|n| {
            let norm = n.replace('\\', "/");
            norm.eq_ignore_ascii_case(want)
                || (bitness != 64 && norm.to_ascii_lowercase().ends_with("/ms/x86/d3d9.dll"))
                || (bitness == 64 && norm.to_ascii_lowercase().ends_with("/ms/x64/d3d9.dll"))
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("dgVoodoo zip does not contain {want} — unexpected release layout")
        })?;
    let dest = game::join_ci(game_dir, &["d3d9.dll"]);
    // Refuse to clobber a foreign wrapper; callers should have blocked Install already.
    if dest.is_file() && !game::is_dgvoodoo(game_dir) {
        bail!(
            "a d3d9.dll that is not dgVoodoo is already present; remove or replace it, then Install again"
        );
    }
    let had_conf = game_dir.join(game::DGVOODOO_CONF).is_file();
    net::extract_member(&mut zip, &member, &dest)?;
    write_dgvoodoo_conf(game_dir)?;
    if !game::is_dgvoodoo(game_dir) {
        bail!("wrote d3d9.dll + dgVoodoo.conf but dgVoodoo was not detected afterward");
    }
    // Record what this tool put there so Remove can take it away again (#91).
    let mut marker = format!("{DGVOODOO_TAG}\n");
    if !had_conf {
        marker.push_str("conf-ours\n");
    }
    fs::write(game_dir.join(game::DGVOODOO_MARKER), marker)?;
    Ok(vec!["d3d9.dll".into(), "dgVoodoo.conf".into()])
}

fn step_dgvoodoo(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let mut out: Vec<String> = Vec::new();
    let member = dgvoodoo_d3d9_member(st.bitness);
    if game::is_dgvoodoo(d) {
        progress(50, "dgVoodoo DLL present — merging conf");
    } else {
        // Do not treat d3d9.dll.off (old ReShade) as dgVoodoo — download the real DLL.
        if game::join_ci(d, &["d3d9.dll"]).is_file() {
            bail!(
                "a d3d9.dll that is not dgVoodoo is already present; remove or replace it with \
                 dgVoodoo 2.87.3 ({member}), then Install again"
            );
        }
        progress(0, &format!("Downloading dgVoodoo {DGVOODOO_TAG}"));
        let z = work.join("dgVoodoo2_87_3.zip");
        net::download(client, DGVOODOO_ZIP, &z, "dgVoodoo 2.87.3", progress)?;
        progress(90, &format!("Extracting {member}"));
        out.extend(install_dgvoodoo_from_zip(&z, d, st.bitness)?);
        progress(100, "dgVoodoo 2.87.3 ready");
        return Ok(out);
    }
    // DLL already there: merge conf so VRAM/OutputAPI stay safe without wiping CPL.
    write_dgvoodoo_conf(d)?;
    out.push("dgVoodoo.conf (OutputAPI/VRAM merged)".into());
    progress(100, "dgVoodoo conf merged");
    Ok(out)
}

fn step_reshade(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let proxy = game::join_ci(d, &[game::RESHADE_PROXY]);
    if !st.reshade && proxy.is_file() {
        bail!(
            "{} exists but is not ReShade (DXVK, Special K, another injector?). Remove it first.",
            game::RESHADE_PROXY
        );
    }
    // A proxy of the wrong bitness is invisible from inside the game: the
    // loader simply does not load it, ReShade writes no log, and the Home key
    // does nothing. Whatever the marker says, that one gets replaced (#69).
    let wrong_bitness = st.reshade && game::exe_bitness(&proxy).is_ok_and(|b| b != st.bitness);
    progress(0, "Looking up latest ReShade");
    let (ver, url) = resolve_reshade_setup(client)?;
    if st.reshade && !wrong_bitness {
        // Only a copy this tool placed is refreshed; a user's own ReShade stays.
        match fs::read_to_string(d.join(game::RESHADE_MARKER)) {
            Ok(mine) if mine.trim() == ver => {
                return Ok(vec![format!("ReShade already current ({ver})")]);
            }
            Ok(_) => progress(0, &format!("ReShade {ver} is out, refreshing")),
            Err(_) => {
                return Ok(vec![
                    "ReShade present (not placed by this tool, left as is)".to_owned(),
                ]);
            }
        }
    }
    let setup = work.join(format!("ReShade_Setup_{ver}_Addon.exe"));
    net::download(client, &url, &setup, "ReShade", progress)?;
    let mut out = install_reshade_from_setup(&setup, d, st.bitness, game::RESHADE_PROXY)?;
    fs::write(d.join(game::RESHADE_MARKER), ver.as_bytes())?;
    if wrong_bitness {
        out.push(format!(
            "{} was {}-bit in a {}-bit game and has been replaced",
            game::RESHADE_PROXY,
            if st.bitness == 32 { 64 } else { 32 },
            st.bitness
        ));
        // The 64-bit add-on cannot belong to a 32-bit game either; it came from
        // the same mistaken install and ReShade would keep trying to load it.
        let stray = d.join(game::FEEDER_ADDON);
        if st.is32() && stray.is_file() {
            fs::remove_file(&stray)?;
            out.push(format!(
                "{} removed (64-bit add-on in a 32-bit game)",
                game::FEEDER_ADDON
            ));
        }
    }
    Ok(out)
}

// ── step 2: ReShade shader headers ────────────────────────────────

fn step_headers(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let shaders = game::join_ci(st.game_dir(), &["reshade-shaders", "Shaders"]);
    let mut installed = Vec::new();
    for h in game::RESHADE_HEADERS {
        let dest = game::existing_ci(&shaders, h).unwrap_or_else(|| shaders.join(h));
        if dest.is_file() {
            continue;
        }
        net::download(
            client,
            &format!("{RESHADE_SHADERS_RAW}{h}"),
            &dest,
            h,
            progress,
        )?;
        installed.push(format!("reshade-shaders/Shaders/{h}"));
    }
    if installed.is_empty() {
        progress(100, "ReShade shader headers already present");
    }
    Ok(installed)
}

// ── step 3: DLSS5-Feeder ───────────────────────────────────────────

/// A release whose tag says beta or rc. Upstream does not flag all of them
/// as prereleases, so the name is what the install log goes by.
fn is_prerelease_tag(tag: &str) -> bool {
    let t = tag.to_ascii_lowercase();
    t.contains("beta") || t.contains("-rc") || t.contains("alpha")
}

fn step_feeder(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    // An installed Feeder used to be left alone forever (a 0.7.0 survived every
    // reinstall while 0.12.0 was out, #6). The zip is small: fetch it and
    // compare the add-on's size with what is on disk.
    progress(0, "Looking up latest DLSS5-Feeder");
    // Since 0.11 the project ships one zip per release instead of loose assets;
    // the file name carries the version, so the tag is read first.
    //
    // Whatever upstream marks as the latest release is what gets installed,
    // including a tag named "-beta": that project publishes builds it means
    // people to run with prerelease=false (v0.13.1-beta.1, v0.12.1-beta.2)
    // while flagging the ones it does not (v0.13.0-beta.1). Those carry
    // fixes the stable v0.12.0 lacks. The name is reported, so a beta is
    // never installed silently.
    let tag = match net::latest_tag(client, FEEDER_REPO) {
        Ok(t) => t,
        Err(_) => net::github_release_tags_html(client, FEEDER_REPO, "v", 1)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no DLSS5-Feeder release found"))?,
    };
    let tag = &tag;
    let note = if is_prerelease_tag(tag) {
        " (beta)"
    } else {
        ""
    };
    let url = net::github_asset_url_html(client, FEEDER_REPO, tag, r#"[^"]+\.zip"#)?;
    let zip_path = work.join("dlss5-feeder.zip");
    net::download(client, &url, &zip_path, "DLSS5-Feeder", progress)?;

    let d = st.game_dir();
    let f = fs::File::open(&zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("DLSS5-Feeder download is not a valid zip")?;
    let members: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let pick = |want: &str| -> Option<String> {
        members
            .iter()
            .find(|m| net::file_name(&m.replace('\\', "/")).eq_ignore_ascii_case(want))
            .cloned()
    };
    // 32-bit: the in-game half is addon32 and the 64-bit helper exe goes to
    // host64\; both must come from the same zip (helper protocol).
    let addon_name = if st.is32() {
        game::FEEDER_ADDON32
    } else {
        game::FEEDER_ADDON
    };
    let addon =
        pick(addon_name).ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {addon_name}"))?;
    let fx = pick(game::FEEDER_FX)
        .ok_or_else(|| anyhow!("DLSS5-Feeder {tag} has no {}", game::FEEDER_FX))?;
    let host_member = st.is32().then(|| pick(game::HOST_EXE)).flatten();
    if st.is32() && host_member.is_none() {
        bail!("DLSS5-Feeder {tag} has no {}", game::HOST_EXE);
    }
    // The 32-bit halves talk a versioned IPC protocol to each other, and a
    // mismatch is fatal at runtime: "the game add-on speaks protocol v8, this
    // host v9 -- the two halves are from different releases" and the host exits
    // (#69). Sizes cannot see that, so the tag each half was taken from is
    // recorded and both are replaced unless both markers name this release.
    let marker_says = |dir: &Path| -> bool {
        fs::read_to_string(dir.join(game::FEEDER_MARKER)).is_ok_and(|m| m.trim() == tag.as_str())
    };
    let halves_agree = !st.is32() || marker_says(&st.consumer_dir());
    let host_current = match &host_member {
        Some(m) => same_size(&mut zip, m, &st.consumer_dir().join(game::HOST_EXE)),
        None => true,
    };
    if st.feeder
        && halves_agree
        && marker_says(d)
        && host_current
        && same_size(&mut zip, &addon, &d.join(addon_name))
    {
        return Ok(vec![format!("DLSS5-Feeder already current ({tag}{note})")]);
    }
    net::extract_member(&mut zip, &addon, &d.join(addon_name))?;
    fs::write(d.join(game::FEEDER_MARKER), tag.as_bytes())?;
    let mut out = vec![format!("{addon_name} ({tag}{note})")];
    if let Some(m) = &host_member {
        let host = st.consumer_dir();
        fs::create_dir_all(&host)?;
        net::extract_member(&mut zip, m, &host.join(game::HOST_EXE))?;
        // Both halves now carry the tag they came from, so a later install can
        // tell "same release" from "same size".
        fs::write(host.join(game::FEEDER_MARKER), tag.as_bytes())?;
        out.push(format!(
            "{}/{} ({tag}{note})",
            game::HOST_DIR,
            game::HOST_EXE
        ));
    }
    let shaders = game::join_ci(d, &["reshade-shaders", "Shaders"]);
    net::extract_member(&mut zip, &fx, &game::join_ci(&shaders, &[game::FEEDER_FX]))?;
    out.push(format!("reshade-shaders/Shaders/{}", game::FEEDER_FX));
    Ok(out)
}

// ── step 4: LumeniteFX ─────────────────────────────────────────────

pub fn install_lumenite_from_zip(zip_path: &Path, game_dir: &Path) -> Result<Vec<String>> {
    let shaders = game::join_ci(game_dir, &["reshade-shaders", "Shaders"]);
    let textures = game::join_ci(game_dir, &["reshade-shaders", "Textures"]);
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("LumeniteFX download is not a valid zip")?;
    let fx = net::members_matching(
        &zip,
        &Regex::new(r"(?i)/Shaders/lumenite_[^/]+\.fx$").unwrap(),
    );
    let fxh = net::members_matching(
        &zip,
        &Regex::new(r"(?i)/Shaders/include/[^/]+\.fxh$").unwrap(),
    );
    let png = net::members_matching(
        &zip,
        &Regex::new(r"(?i)/Textures/lumenite_bluenoise256\.png$").unwrap(),
    );
    if fx.is_empty() || png.is_empty() {
        bail!("LumeniteFX archive layout changed; shaders or texture not found");
    }
    let mut installed = Vec::new();
    for (members, dir, rel) in [
        (&fx, shaders.clone(), "reshade-shaders/Shaders"),
        (
            &fxh,
            shaders.join("include"),
            "reshade-shaders/Shaders/include",
        ),
        (&png, textures, "reshade-shaders/Textures"),
    ] {
        for m in members {
            let name = net::file_name(m);
            net::extract_member(&mut zip, m, &dir.join(name))?;
            installed.push(format!("{rel}/{name}"));
        }
    }
    if let Some(msg) = apply_traa_ui_patch(game_dir)? {
        installed.push(msg);
    }
    Ok(installed)
}

fn step_lumenite(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if st.lumenite {
        progress(100, "LumeniteFX already installed");
        let mut out = vec![];
        if let Some(msg) = apply_traa_ui_patch(st.game_dir())? {
            out.push(msg);
        }
        return Ok(out);
    }
    let z = work.join("LumeniteFX.zip");
    net::download(client, LUMENITE_ZIP, &z, "LumeniteFX", progress)?;
    install_lumenite_from_zip(&z, st.game_dir())
}

// ── step 5: DLSS 5 add-on + models ─────────────────────────────────

/// True when `dest` exists with the uncompressed size of `member`. Cheap
/// "is this the same build" check for files whose names carry no version.
pub fn same_size<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    member: &str,
    dest: &Path,
) -> bool {
    let local = fs::metadata(dest).map(|m| m.len()).ok();
    let remote = zip.by_name(member).ok().map(|f| f.size());
    local.is_some() && local == remote
}

pub fn install_single_from_zip(zip_path: &Path, member_name: &str, dest: &Path) -> Result<()> {
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f)
        .with_context(|| format!("{} is not a valid zip", zip_path.display()))?;
    let hit = zip
        .file_names()
        .find(|n| net::file_name(n).eq_ignore_ascii_case(member_name))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{} does not contain {member_name}", zip_path.display()))?;
    net::extract_member(&mut zip, &hit, dest)
}

fn step_dlss5(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    // A game with its own DLSS keeps its own nvngx_dlss.dll.
    let dlss_present = st.dlss || st.mode == game::Mode::Native;
    // Every piece is re-checked: the add-on by comparing its (small) zip, the
    // two NVIDIA DLLs by the release tag recorded when this tool placed them.
    // A DLL without a marker is the game's or the user's and is left alone.
    let plan = [
        (
            "renodx-dlss5-",
            game::DLSS5_ADDON,
            false,
            Some(game::DLSS5_ADDON_MARKER),
        ),
        (
            "dlssnr-",
            game::DLSSNR_DLL,
            st.dlssnr,
            Some(game::DLSSNR_MARKER),
        ),
        (
            "dlss-",
            game::DLSS_DLL,
            dlss_present,
            Some(game::DLSS_MARKER),
        ),
    ];
    progress(0, "Looking up DLSS 5 add-on releases");
    let cdir = st.consumer_dir();
    fs::create_dir_all(&cdir)?;
    // Which renodx-dlss5 build. One named in DLSS5ONECLICK_RENODX_TAG is taken
    // as is (rhi_latest). Otherwise the classic line when the user ticked it;
    // when the feeder step_feeder just placed (its marker carries the tag) is
    // too old for anything newer; or when this machine's Feeder host has
    // already measured the newer build faulting in the current driver --
    // fetching that build again only reproduces it (#69). The last holds for a
    // 32-bit game too: its add-on lives in host64\ and is fetched by this same
    // loop, and its host is what prints the verdict (sempie27's GTA IV kept
    // getting v4.7 back while its own log said v4.7 faults, #69).
    let named = std::env::var_os(RENODX_TAG_ENV).is_some_and(|v| !v.is_empty());
    let feeder_tag = fs::read_to_string(game::join_ci(st.game_dir(), &[game::FEEDER_MARKER]))
        .ok()
        .map(|s| s.trim().to_owned());
    // Read, and recorded against the driver, even when something else decides:
    // a later install without the tick still knows.
    let measured = addon_faulted_in_driver(&cdir);
    let classic = if named {
        None
    } else if install_extras().classic_addon {
        Some("chosen in the app")
    } else if feeder_needs_classic(feeder_tag.as_deref()) {
        Some("the feeder installed here predates the newer add-on builds")
    } else if measured {
        Some("this game's log reports the newer build faulting in the driver")
    } else {
        None
    };
    let mut installed = Vec::new();
    if let Some(why) = classic {
        installed.push(format!("{RENODX_CLASSIC_TAG}: {why}"));
    }
    for (prefix, fname, present, marker) in plan {
        let pin = match classic {
            Some(why) if prefix == "renodx-dlss5-" => {
                progress(0, &format!("Holding {fname} on {CLASSIC_LINE}: {why}"));
                Some(CLASSIC_LINE)
            }
            _ => None,
        };
        let (tag, url) = rhi_pinned(client, prefix, pin)?;
        if present {
            match marker.map(|m| fs::read_to_string(cdir.join(m))) {
                Some(Ok(mine)) if mine.trim() == tag => {
                    installed.push(format!("{fname} already current ({tag})"));
                    continue;
                }
                Some(Ok(_)) => progress(0, &format!("{fname}: {tag} is out, refreshing")),
                _ => {
                    installed.push(format!("{fname} present (not placed by this tool)"));
                    continue;
                }
            }
        }
        let z = work.join(format!("{tag}.zip"));
        net::download(client, &url, &z, fname, progress)?;
        let dest = cdir.join(fname);
        if fname == game::DLSS5_ADDON && st.dlss5_addon {
            let f = fs::File::open(&z)?;
            let mut zip =
                zip::ZipArchive::new(f).context("DLSS 5 add-on download is not a valid zip")?;
            let hit = zip
                .file_names()
                .find(|n| net::file_name(n).eq_ignore_ascii_case(fname))
                .map(str::to_owned);
            if hit.is_some_and(|h| same_size(&mut zip, &h, &dest)) {
                // Record the tag even when nothing is copied: every build of
                // this add-on carries the same FileVersion, so the tag on disk
                // is the only way anything afterwards can name the build.
                if let Some(m) = marker {
                    let _ = fs::write(cdir.join(m), tag.as_bytes());
                }
                installed.push(format!("{fname} already current ({tag})"));
                continue;
            }
        }
        install_single_from_zip(&z, fname, &dest)?;
        if fname == game::DLSSNR_DLL {
            if let Err(e) = validate_dlssnr(&dest) {
                let _ = fs::remove_file(&dest);
                return Err(e);
            }
        }
        if let Some(m) = marker {
            fs::write(cdir.join(m), tag.as_bytes())?;
        }
        let shown = if st.is32() {
            format!("{}/{fname} ({tag})", game::HOST_DIR)
        } else {
            format!("{fname} ({tag})")
        };
        installed.push(shown);
    }
    Ok(installed)
}

// ── opti engine: just the model DLL beside OptiScaler ───────────────

fn step_dlssnr_only(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    progress(0, "Looking up DLSS 5 model releases");
    let (tag, url) = rhi_latest(client, "dlssnr-")?;
    if st.dlssnr {
        match fs::read_to_string(st.game_dir().join(game::DLSSNR_MARKER)) {
            Ok(mine) if mine.trim() == tag => {
                return Ok(vec![format!(
                    "{} already current ({tag})",
                    game::DLSSNR_DLL
                )]);
            }
            Ok(_) => progress(
                0,
                &format!("{}: {tag} is out, refreshing", game::DLSSNR_DLL),
            ),
            Err(_) => {
                return Ok(vec![format!(
                    "{} present (not placed by this tool)",
                    game::DLSSNR_DLL
                )]);
            }
        }
    }
    let z = work.join(format!("{tag}.zip"));
    net::download(client, &url, &z, game::DLSSNR_DLL, progress)?;
    let dest = st.game_dir().join(game::DLSSNR_DLL);
    install_single_from_zip(&z, game::DLSSNR_DLL, &dest)?;
    if let Err(e) = validate_dlssnr(&dest) {
        let _ = fs::remove_file(&dest);
        return Err(e);
    }
    fs::write(st.game_dir().join(game::DLSSNR_MARKER), tag.as_bytes())?;
    Ok(vec![format!("{} ({tag})", game::DLSSNR_DLL)])
}

// ── native mode: a Feeder left over from an earlier install must go ─

fn step_feeder_cleanup(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let mut removed = Vec::new();
    let shaders = game::join_ci(d, &["reshade-shaders", "Shaders"]);
    for f in [
        game::join_ci(d, &[game::FEEDER_ADDON]),
        game::join_ci(&shaders, &[game::FEEDER_FX]),
    ] {
        if f.is_file() {
            fs::remove_file(&f)?;
            removed.push(
                f.strip_prefix(d)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    reshade_ini::remove_our_techniques(d)?;
    progress(
        100,
        "DLSS5-Feeder removed; the add-on hooks the game's own DLSS",
    );
    Ok(removed)
}

/// The plan already declines to *install* the RenoDX add-on on the Neural
/// Upstream route, but nothing removed one an earlier run had placed. Anyone who
/// installed once without the box and again with it ends up with both, and
/// ReShade loads every add-on it finds.
///
/// They are not additive. Both detour `NVSDK_NGX_D3D12_CreateFeature` and
/// `EvaluateFeature`, and both create NGX feature 18 on the same device. The
/// second create is refused, so the user gets no neural rendering at all rather
/// than one of the two implementations.
fn step_dlss5_cleanup(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let f = st.consumer_dir().join(game::DLSS5_ADDON);
    if f.is_file() {
        fs::remove_file(&f)?;
        removed.push(game::DLSS5_ADDON.to_owned());
        progress(
            100,
            "RenoDX DLSS 5 add-on removed; Neural Upstream replaces it",
        );
    } else {
        progress(100, "no RenoDX DLSS 5 add-on to remove");
    }
    Ok(removed)
}

// ── step 5b: DX11 bridge (native-DLSS games rendering with D3D11) ──

fn step_bridge(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let dest = st.game_dir().join(game::BRIDGE_ADDON);
    // The bridge has no version tag in its file name and its releases fix
    // add-on-specific behaviour (1.4.0: the 2026-08-28 add-on build), so an
    // existing copy is refreshed whenever the published file differs in size.
    if st.bridge && dest.is_file() {
        let local = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        match net::remote_len(client, BRIDGE_DOWNLOAD) {
            Ok(Some(remote)) if remote != local => {
                progress(0, "dlss5-bridge changed upstream, refreshing");
            }
            Ok(_) => {
                return Ok(vec!["dlss5-bridge.addon64 already current".to_owned()]);
            }
            Err(_) => {
                return Ok(vec![
                    "dlss5-bridge.addon64 present (could not check for a newer one)".to_owned(),
                ]);
            }
        }
    } else {
        progress(0, "Fetching latest dlss5-bridge");
    }
    net::download(client, BRIDGE_DOWNLOAD, &dest, game::BRIDGE_ADDON, progress)?;
    Ok(vec![game::BRIDGE_ADDON.into()])
}

/// The RTX 40 MFG add-on, fetched only when the tick asked for it.
///
/// Like the bridge it carries no version in its file name, so an existing copy
/// is refreshed whenever the published file differs in size.
fn step_mfg(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let dest = st.game_dir().join(game::MFG_ADDON);
    if st.mfg && dest.is_file() {
        let local = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        match net::remote_len(client, MFG_DOWNLOAD) {
            Ok(Some(remote)) if remote != local => {
                progress(0, "MFG unlock changed upstream, refreshing");
            }
            Ok(_) => {
                return Ok(vec![format!("{} already current", game::MFG_ADDON)]);
            }
            Err(_) => {
                return Ok(vec![format!(
                    "{} present (could not check for a newer one)",
                    game::MFG_ADDON
                )]);
            }
        }
    } else {
        progress(0, "Fetching the RTX 40 MFG unlock");
    }
    net::download(client, MFG_DOWNLOAD, &dest, game::MFG_ADDON, progress)?;
    let mut done = vec![game::MFG_ADDON.to_owned()];
    done.extend(mfg_provider(client, st, work, progress)?);
    Ok(done)
}

/// The frame-generation provider the MFG add-on will accept.
///
/// Version 0.9 validates the provider by build and refuses the rest: a reporter
/// with an RTX 4060 got "Validated provider result: unsupported/unknown" and no
/// effect at all, on a game whose own menu offered 2X-6X (#90). The add-on's
/// README says to use the newest `nvngx_dlssg.dll`; rhi-repo publishes it, the
/// same place this tool already takes `nvngx_dlss.dll` and the neural model
/// from, so Install can place it without asking anyone to fetch a DLL.
///
/// A provider the game shipped is moved to `.original` rather than overwritten,
/// and Remove puts it back.
fn mfg_provider(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let d = st.game_dir();
    let dest = d.join(game::DLSSG_DLL);
    let marker = d.join(game::DLSSG_MARKER);
    progress(0, "Looking up frame-generation runtime releases");
    let (tag, url) = rhi_latest(client, "dlssg-")?;
    if fs::read_to_string(&marker).is_ok_and(|t| t.trim() == tag) {
        return Ok(vec![format!("{} already current ({tag})", game::DLSSG_DLL)]);
    }
    let backup = d.join(game::DLSSG_BACKUP);
    if dest.is_file() && !marker.is_file() && !backup.is_file() {
        fs::rename(&dest, &backup)?;
    }
    let z = work.join(format!("{tag}.zip"));
    net::download(client, &url, &z, game::DLSSG_DLL, progress)?;
    install_single_from_zip(&z, game::DLSSG_DLL, &dest)?;
    fs::write(&marker, tag.as_bytes())?;
    Ok(vec![format!("{} ({tag})", game::DLSSG_DLL)])
}

// ── step 5c: neural-upstream (experimental consumer, native DLSS only) ──

fn step_upstream(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let dest = st.game_dir().join(game::UPSTREAM_ADDON);
    // Like the bridge, its releases carry no tag in the file name, so an
    // existing copy is refreshed whenever the published file differs in size.
    if st.upstream && dest.is_file() {
        let local = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        match net::remote_len(client, UPSTREAM_DOWNLOAD) {
            Ok(Some(remote)) if remote != local => {
                progress(0, "neural-upstream changed upstream, refreshing");
            }
            Ok(_) => {
                return Ok(vec![format!("{} already current", game::UPSTREAM_ADDON)]);
            }
            Err(_) => {
                return Ok(vec![format!(
                    "{} present (could not check for a newer one)",
                    game::UPSTREAM_ADDON
                )]);
            }
        }
    } else {
        progress(0, "Fetching latest neural-upstream");
    }
    net::download(
        client,
        UPSTREAM_DOWNLOAD,
        &dest,
        game::UPSTREAM_ADDON,
        progress,
    )?;
    let mut done = vec![game::UPSTREAM_ADDON.to_owned()];
    // The add-on reads its strength from ReShade.ini at startup, so the choice
    // can be made here instead of only in the in-game overlay (#68).
    let preset = upstream_preset();
    if preset != 0 {
        reshade_ini::write_upstream_preset(st.game_dir(), preset)?;
        if let Some((name, ..)) = reshade_ini::UPSTREAM_PRESETS
            .iter()
            .find(|(_, id, _)| *id == preset)
        {
            done.push(format!("neural-upstream preset: {name}"));
        }
    }
    Ok(done)
}

/// RTX 40 multi-frame generation, for a scripted install: read once by the CLI
/// into `Extras::ada_mfg` -- nothing writes it at runtime.
pub const ADA_MFG_ENV: &str = "DLSS5ONECLICK_ADA_MFG";

/// True when `DLSS5ONECLICK_ADA_MFG` asks for the RTX 40 MFG unlock.
pub fn ada_mfg_from_env() -> bool {
    std::env::var_os(ADA_MFG_ENV).is_some()
}

/// `"true"` when the install running on this thread asked for the RTX 40 MFG
/// unlock, else `"false"`: the value `[FrameGen] AdaMfgUnlock` takes.
///
/// The fork's own note: "Optional built-in y4my4my4m RTX 40 MFG unlock.
/// Memory-only, supported runtimes only." Memory-only is what makes it
/// offerable here — there is no second download and nothing for the user to
/// place by hand.
fn ada_mfg() -> &'static str {
    if install_extras().ada_mfg {
        "true"
    } else {
        "false"
    }
}

/// Which neural-upstream strength preset a scripted install seeds; read once
/// by the CLI into `Extras::upstream_preset` -- nothing writes it at runtime.
pub const UPSTREAM_PRESET_ENV: &str = "DLSS5ONECLICK_UPSTREAM_PRESET";

/// `DLSS5ONECLICK_UPSTREAM_PRESET` as a number; 0 when unset or not one.
pub fn upstream_preset_from_env() -> u8 {
    std::env::var(UPSTREAM_PRESET_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// The preset the running install asked for when it is a real one; 0 leaves
/// the overlay's own.
fn upstream_preset() -> u8 {
    let p = install_extras().upstream_preset;
    if reshade_ini::UPSTREAM_PRESETS
        .iter()
        .any(|(_, id, _)| *id == p)
    {
        p
    } else {
        0
    }
}

thread_local! {
    /// The install running on this thread: its resolved quality preset and
    /// the extras it was asked for. Steps are plain fn pointers, so this is
    /// how a choice reaches one -- never the process environment, which the
    /// GUI would have to write while its other threads read it (setenv is not
    /// thread-safe on glibc). Per thread, so a GUI worker and parallel tests
    /// never see each other's install.
    static INSTALL: std::cell::RefCell<Option<(ResolvedQuality, Extras)>> =
        const { std::cell::RefCell::new(None) };
}

/// Holds this thread's install context for one run and clears it however the
/// run ends, early `?` returns included.
struct InstallScope;

impl InstallScope {
    fn enter(quality: ResolvedQuality, x: Extras) -> Self {
        INSTALL.with(|c| *c.borrow_mut() = Some((quality, x)));
        InstallScope
    }
}

impl Drop for InstallScope {
    fn drop(&mut self) {
        INSTALL.with(|c| *c.borrow_mut() = None);
    }
}

/// Quality resolution for the install running on this thread.
fn install_quality() -> ResolvedQuality {
    INSTALL
        .with(|c| c.borrow().as_ref().map(|(q, _)| q.clone()))
        .unwrap_or_else(quality_preset::fallback_medium)
}

/// Extras of the install running on this thread; all off outside one.
fn install_extras() -> Extras {
    INSTALL
        .with(|c| c.borrow().as_ref().map(|(_, x)| *x))
        .unwrap_or_default()
}

/// True when this game has already refused a reduced work resolution.
///
/// Some games cannot create the staging SRV for the smaller image at any size
/// below full — Dying Light fails identically at 90% and 85% and works at 100%.
/// The feed retries three times and stops, before any DLSS create, so the whole
/// install goes quiet and nothing on screen says why (#74).
pub fn work_resolution_refused(game_dir: &Path) -> bool {
    let Ok(log) = fs::read_to_string(game_dir.join("dlss5-feed.log")) else {
        return false;
    };
    if !log.contains("work-resolution staging SRV failed") {
        return false;
    }
    // Feeder 0.15.0 fixed the cause: the staging copy was created in the
    // backbuffer's exact ..._UNORM_SRGB format and then viewed as ..._UNORM,
    // which a D3D11 view may not do unless the resource is typeless, so every
    // reduced work resolution failed on an sRGB swapchain (DLSS5-Feeder#85).
    // A failure logged by an older build says nothing about the one this very
    // install is about to put in the folder, so it must not hold the setting
    // down forever.
    feeder_log_version(&log).is_none_or(|v| v >= FEEDER_SRGB_FIX)
}

/// First Feeder release where a reduced work resolution works on an sRGB
/// swapchain.
const FEEDER_SRGB_FIX: [u64; 3] = [0, 15, 0];

/// `"HH:MM:SS.mmm  dlss5-feed 0.15.0 (built ...) attached."` -> `[0, 15, 0]`.
fn feeder_log_version(log: &str) -> Option<[u64; 3]> {
    let mut it = log.lines().next()?.split_whitespace();
    it.find(|t| t.starts_with("dlss5-feed"))?;
    let raw = it.next()?;
    let mut parts = raw.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0));
    let v = [
        parts.next()?,
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    ];
    raw.chars().next()?.is_ascii_digit().then_some(v)
}

/// True when this game's own logs say the DLSS 5 add-on faulted inside the
/// driver's NGX runtime.
///
/// The Feeder's host prints that verdict itself, having measured it: the neural
/// evaluate takes an access violation in `D3D12Core.dll` reached through
/// `nvngx_dlssnr.dll`, so DLSS 5 delivers nothing while everything else keeps
/// working. It names the classic add-on build as one that passes there. Read
/// the machine's own evidence rather than assuming it from a driver number —
/// the measurement covers 616.64, and newer drivers are untested (#69).
/// Where the driver verdict is remembered once a log has stated it. A game
/// folder is the wrong place to keep it: Remove empties the folder, a fresh
/// game has no log yet, and the fault belongs to the driver rather than to any
/// one game.
fn driver_fault_flag() -> PathBuf {
    crate::settings::Settings::path().with_file_name("driver-fault.txt")
}

pub fn addon_faulted_in_driver(consumer_dir: &Path) -> bool {
    faulted_in_driver_at(
        consumer_dir,
        &driver_fault_flag(),
        crate::gpu::nvidia_driver(),
    )
}

/// The decision itself, with its two pieces of state passed in so a test can
/// exercise it without touching the machine's own file or its real driver.
fn faulted_in_driver_at(consumer_dir: &Path, flag: &Path, driver: Option<String>) -> bool {
    // On a 32-bit game the feed's log is beside the exe and the host's is in
    // host64\; on a 64-bit one both are the same folder. Check the pair either
    // way rather than assuming which layout this is.
    let dirs = [consumer_dir.to_path_buf(), consumer_dir.join("..")];
    let in_logs = dirs.iter().any(|d| {
        ["dlss5-feed.log", "dlss5-feed-host.log"].iter().any(|n| {
            fs::read_to_string(d.join(n))
                .is_ok_and(|l| l.contains("is a combination measured to fail"))
        })
    });
    let Some(drv) = driver else {
        return in_logs;
    };
    if in_logs {
        // Record it against the driver that produced it, so a driver update
        // retires the verdict by itself.
        if let Some(dir) = flag.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(flag, drv.as_bytes());
        return true;
    }
    match fs::read_to_string(flag) {
        Ok(seen) if seen.trim() == drv => true,
        Ok(_) => {
            let _ = fs::remove_file(flag);
            false
        }
        Err(_) => false,
    }
}

pub fn write_feeder_cfg(game_dir: &Path, r: &ResolvedQuality) -> Result<()> {
    let path = game::join_ci(game_dir, &[crate::feeder_cfg::CFG_NAME]);
    // A preset that seeds a reduced work resolution would otherwise put this
    // game straight back into the failure it just came out of, every install.
    let mut r = r.clone();
    if r.work_resolution < 100 && work_resolution_refused(game_dir) {
        r.work_resolution = 100;
        r.work_upscale = 0;
        r.summary = format!(
            "{} - work_resolution held at 100% (this game refused a smaller one)",
            r.summary
        );
    }
    let r = &r;
    // A fresh cfg gets the full layout. An existing one keeps everything the
    // preset does not decide -- a raised create_delay (diagnose's own advice
    // for a crash in the add-on), the HDR / depth / motion-vector fixes a game
    // needed, comments -- so reinstalling or updating does not undo tuning.
    let mut text = match fs::read_to_string(&path) {
        Ok(prev) => quality_preset::apply_preset_to_cfg(&prev, r),
        Err(_) => quality_preset::feeder_cfg_text(r),
    };
    // Overlay UX defaults from Settings (log_detail / evaluate_stride / …).
    let settings = crate::settings::Settings::load();
    text = crate::settings::apply_overlay_to_cfg(&text, &settings);
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ── RTX Remix: download a complete mod for a game that has one ──────

/// Fetch and lay in a complete RTX Remix mod (runtime + assets) from its GitHub
/// release, for a game the catalogue matched. This does not install DLSS 5 —
/// it makes the game a Remix game; the Remix route then installs the model.
/// Refuses when a `.trex` runtime is already present, and when the release
/// carries no complete runtime (source or a bare proxy) — the caller shows the
/// link instead.
pub fn install_remix_mod(
    client: &Client,
    repo: &str,
    game_dir: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if crate::remix::find_runtime(game_dir).is_some() {
        bail!(
            "This game already has an RTX Remix runtime (.trex) — remove the existing mod first, \
             or just install DLSS 5 into it."
        );
    }
    progress(0, "Looking up the RTX Remix mod release");
    let tag = net::latest_tag(client, repo).map_err(|_| {
        anyhow!("{repo} publishes no installable release — open its page and add the mod by hand.")
    })?;
    let url = net::github_asset_url_html(client, repo, &tag, r#"[^"]+\.zip"#).map_err(|_| {
        anyhow!("{repo} {tag} has no downloadable .zip — open its page and add the mod by hand.")
    })?;
    let work = tempfile::Builder::new()
        .prefix("dlss5oneclick-remix-")
        .tempdir()?;
    let zip_path = work.path().join("remix-mod.zip");
    net::download(client, &url, &zip_path, "RTX Remix mod", progress)?;
    progress(50, "Extracting the RTX Remix mod");
    place_remix_mod(&zip_path, game_dir, &format!("{repo} {tag}"))
}

/// Extract a complete Remix mod's runtime subtree from `zip_path` into
/// `game_dir` (backing up any game file it replaces) and record a manifest.
/// Split out from the download so the extraction is testable on a fixture zip.
fn place_remix_mod(zip_path: &Path, game_dir: &Path, header: &str) -> Result<Vec<String>> {
    let f = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f).context("the RTX Remix mod download is not a valid zip")?;
    let members: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let root = crate::remix::mod_root(&members).ok_or_else(|| {
        anyhow!(
            "This release carries no complete Remix runtime (a `.trex` folder) — it is source or a \
             proxy that needs manual steps. Open the mod's page for the full download."
        )
    })?;
    let mut written: Vec<String> = Vec::new();
    for member in &members {
        let norm = member.replace('\\', "/");
        if norm.ends_with('/') {
            continue; // directory entry
        }
        let Some(rel) = crate::remix::strip_root(&norm, &root) else {
            continue;
        };
        let parts: Vec<&str> = rel
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        if parts.is_empty() {
            continue;
        }
        let dest = parts
            .iter()
            .fold(game_dir.to_path_buf(), |acc, p| acc.join(p));
        // Keep any game file we are about to overwrite, so removal restores it.
        if dest.is_file() {
            let bak = dest.with_file_name(format!(
                "{}{REMIX_MOD_ORIG}",
                dest.file_name().unwrap_or_default().to_string_lossy()
            ));
            if !bak.exists() {
                let _ = fs::rename(&dest, &bak);
            }
        }
        net::extract_member(&mut zip, member, &dest)?;
        written.push(parts.join("/"));
    }
    if crate::remix::find_runtime(game_dir).is_none() {
        bail!("Extracted the mod but no .trex runtime landed — the archive layout is unexpected.");
    }
    fs::write(
        game_dir.join(REMIX_MOD_MANIFEST),
        format!("# {header}\n{}\n", written.join("\n")),
    )?;
    Ok(vec![format!(
        "RTX Remix mod installed ({header}, {} files). Now Install DLSS 5 to add neural rendering.",
        written.len()
    )])
}

/// Remove a downloaded RTX Remix mod: delete every file it placed, restore the
/// game files it replaced, and drop the now-empty folders. The base game is
/// left as it was before the mod.
pub fn uninstall_remix_mod(game_dir: &Path) -> Result<Vec<String>> {
    let manifest = game_dir.join(REMIX_MOD_MANIFEST);
    let list = fs::read_to_string(&manifest)
        .map_err(|_| anyhow!("no downloaded RTX Remix mod is recorded in this folder."))?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut count = 0usize;
    for rel in list
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let parts: Vec<&str> = rel
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .collect();
        let p = parts
            .iter()
            .fold(game_dir.to_path_buf(), |acc, x| acc.join(x));
        if p.is_file() {
            fs::remove_file(&p)?;
            count += 1;
        }
        let bak = p.with_file_name(format!(
            "{}{REMIX_MOD_ORIG}",
            p.file_name().unwrap_or_default().to_string_lossy()
        ));
        if bak.is_file() {
            let _ = fs::rename(&bak, &p);
        }
        if let Some(parent) = p.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    // Deepest folders first, so a directory empties before its parent is tried.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for d in dirs {
        let _ = fs::remove_dir(&d); // only succeeds when empty
    }
    let _ = fs::remove_file(&manifest);
    Ok(vec![format!("RTX Remix mod removed ({count} files)")])
}

// ── RTX Remix: model into .trex + one rtx.conf line ─────────────────

fn step_remix(
    client: &Client,
    st: &GameStatus,
    work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let trex = st
        .remix
        .as_ref()
        .ok_or_else(|| anyhow!("not an RTX Remix game (no .trex runtime found)"))?;
    let mut out = Vec::new();

    // 1. The DLSS 5 model, inside the .trex folder, refreshed by tag marker.
    let dest = game::join_ci(trex, &[game::DLSSNR_DLL]);
    let (tag, url) = rhi_latest(client, "dlssnr-")?;
    let marker = trex.join(REMIX_MARKER);
    let current = st.remix_model
        && fs::read_to_string(&marker)
            .map(|t| t.trim() == tag)
            .unwrap_or(false);
    if current {
        out.push(format!("{} already current ({tag})", game::DLSSNR_DLL));
    } else {
        let z = work.join(format!("remix-{tag}.zip"));
        net::download(client, &url, &z, game::DLSSNR_DLL, progress)?;
        install_single_from_zip(&z, game::DLSSNR_DLL, &dest)?;
        if let Err(e) = validate_dlssnr(&dest) {
            let _ = fs::remove_file(&dest);
            return Err(e);
        }
        fs::write(&marker, tag.as_bytes())?;
        out.push(format!("{}/{} ({tag})", ".trex", game::DLSSNR_DLL));
    }

    // 2. Turn the neural pass on in rtx.conf — which key depends on the fork.
    let flavour = crate::remix::flavour(trex);
    match flavour.enable_key() {
        Some(key) => {
            let conf = crate::remix::conf_path(trex);
            let text = fs::read_to_string(&conf).unwrap_or_default();
            let patched = crate::remix::set_option(&text, key, "True");
            if patched != text {
                fs::write(&conf, &patched)?;
                out.push(format!("{key} = True in rtx.conf"));
            } else {
                out.push(format!("{key} already on"));
            }
        }
        None => out.push(
            "this Remix runtime ships no neural pass — reinstall with the runtime swap on \
             (--remix-swap) to replace it with a DLSS 5-capable one"
                .to_owned(),
        ),
    }
    Ok(out)
}

fn step_remix_swap(
    client: &Client,
    st: &GameStatus,
    _work: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    let trex = st
        .remix
        .as_ref()
        .ok_or_else(|| anyhow!("not an RTX Remix game (no .trex runtime found)"))?;
    let mut out = Vec::new();
    for name in REMIX_RUNTIME_ASSETS {
        let dest = game::join_ci(trex, &[name]);
        let bak = dest.with_file_name(format!("{name}{REMIX_ORIG}"));
        // Keep the original once, so uninstall can put the mod's runtime back.
        if dest.is_file() && !bak.exists() {
            fs::copy(&dest, &bak)?;
        }
        net::download(client, &format!("{REMIX_RUNTIME_LATEST}{name}"), &dest, name, progress)?;
        out.push(format!(".trex/{name} (DLSS 5 Remix runtime)"));
    }
    Ok(out)
}

// ── step 6: config ─────────────────────────────────────────────────

fn step_config(_c: &Client, st: &GameStatus, _w: &Path, progress: Progress) -> Result<Vec<String>> {
    reshade_ini::write_reshade_ini(st.game_dir())?;
    reshade_ini::clear_disabled_addons(st.game_dir())?;
    if st.mode == game::Mode::Native {
        progress(100, "ReShade.ini written");
        return Ok(vec![game::RESHADE_INI.into()]);
    }
    let q = install_quality();
    reshade_ini::write_preset(st.game_dir(), q.enable_lumenite)?;
    reshade_ini::write_feed_fx_uniforms(st.game_dir(), &quality_preset::feed_fx_uniforms(&q))?;
    reshade_ini::write_traa_ui_defaults(st.game_dir())?;
    write_feeder_cfg(st.game_dir(), &q)?;
    let mut out = vec![
        game::RESHADE_INI.into(),
        game::RESHADE_PRESET.into(),
        "dlss5-feed.cfg".into(),
    ];
    if q.work_resolution < 100 && work_resolution_refused(st.game_dir()) {
        out.push(
            "work_resolution held at 100%: this game's log shows it refused a smaller one".into(),
        );
    }
    progress(100, "ReShade + feeder defaults (Optimize on first attach)");
    if let Some(msg) = apply_traa_ui_patch(st.game_dir())? {
        out.push(msg);
    }
    Ok(out)
}

// ── step 7: which GPU Windows starts the process on ────────────

/// On a hybrid machine Windows may start the game (or the 32-bit helper) on the
/// iGPU, where NGX does not exist and `NVSDK_NGX_D3D12_Init` answers
/// `0xBAD00001`. That is what a reporter fixed by hand in Settings ▸ System ▸
/// Display ▸ Graphics (#25); this writes the same preference.
fn step_gpu_pref(
    _c: &Client,
    st: &GameStatus,
    _w: &Path,
    progress: Progress,
) -> Result<Vec<String>> {
    if !gpupref::hybrid() {
        progress(100, "one GPU vendor on this machine, nothing to set");
        return Ok(vec![]);
    }
    // On Linux there is no per-application GPU registry — the compositor and
    // PRIME decide which card a process renders on. Writing nothing is the
    // honest outcome; if DLSS later fails with 0xBAD00001, --diagnose names the
    // PRIME offload environment variables (the Linux analog of the Windows
    // high-performance preference). The step stays in the plan on both OSes so
    // the step list is identical; it just no-ops here.
    if cfg!(not(windows)) {
        progress(
            100,
            "hybrid GPU: on Linux the compositor/PRIME choose the GPU — nothing to set",
        );
        return Ok(vec![]);
    }
    let mut targets = vec![st.exe.clone()];
    if st.is32() {
        targets.push(st.consumer_dir().join(game::HOST_EXE));
    }
    let mut out = Vec::new();
    for exe in targets.into_iter().filter(|p| p.is_file()) {
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match gpupref::set_high_performance(&exe) {
            Ok(true) => out.push(format!(
                "{name}: Windows GPU preference set to high performance"
            )),
            Ok(false) => out.push(format!("{name}: already set to the high-performance GPU")),
            Err(e) => out.push(format!("{name}: could not set the GPU preference ({e})")),
        }
    }
    progress(100, "GPU preference checked");
    Ok(out)
}

// ── driver ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Start,
    Done,
    Error,
}

/// Quality seed for an install (Settings page / CLI).
#[derive(Debug, Clone)]
pub struct InstallOpts {
    pub quality: QualityChoice,
    pub overrides: QualityOverrides,
}

impl Default for InstallOpts {
    fn default() -> Self {
        let s = crate::settings::Settings::load();
        Self {
            quality: s.quality_choice(),
            overrides: s.quality_overrides(),
        }
    }
}

pub fn run_all_with(
    exe: &Path,
    engine: Engine,
    x: Extras,
    opts: InstallOpts,
    progress: Progress,
    step_cb: &(dyn Fn(usize, usize, &str, StepState, &str) + Sync),
) -> Result<Vec<(String, Vec<String>)>> {
    let mut st = game::inspect(exe)?;
    if !st.problems.is_empty() {
        bail!("{}", st.problems.join("\n"));
    }
    // The engine constraints below are for the ReShade/OptiScaler routes; a
    // Remix game bypasses them entirely (its plan is the Remix route).
    if st.remix.is_none() {
        if engine == Engine::ReShade {
            if let Some(p) = st.reshade_engine_problem() {
                bail!("{p}");
            }
        }
        if x.upstream && (engine != Engine::ReShade || st.mode != game::Mode::Native) {
            bail!(
                "Neural Upstream runs the network on the colour buffer the game hands its own DLSS, so it needs a game with DLSS of its own on the ReShade engine. This game has none - use the stable ReShade add-on."
            );
        }
        if engine == Engine::Opti && st.is32() {
            bail!("The OptiScaler engine is 64-bit only; a 32-bit game takes the Feeder path.");
        }
        if engine == Engine::Opti && st.mode != game::Mode::Feeder {
            // fine: native DLSS present
        } else if engine == Engine::Opti {
            bail!(
                "The OptiScaler engine needs a game with its own DLSS (its Neural Rendering pass \
                 reads the inputs the game hands to DLSS). This game has none — use the ReShade engine."
            );
        }
    }
    let _run = InstallScope::enter(
        quality_preset::resolve(opts.quality, &st, &opts.overrides),
        x,
    );
    let client = net::client()?;
    let work = tempfile::Builder::new()
        .prefix("dlss5oneclick-")
        .tempdir()?;
    let steps = plan_with(&st, engine, x);
    let n = steps.len();
    let mut results = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        step_cb(i, n, step.name, StepState::Start, "");
        match (step.run)(&client, &st, work.path(), progress) {
            Ok(files) => {
                let detail = if files.is_empty() {
                    "already present".to_owned()
                } else {
                    files.join(", ")
                };
                step_cb(i, n, step.name, StepState::Done, &detail);
                results.push((step.name.to_owned(), files));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                step_cb(i, n, step.name, StepState::Error, &msg);
                return Err(anyhow!("{}: {msg}", step.name));
            }
        }
        st = game::inspect(exe)?;
    }
    // The model-resolution dial is a value, not a plan step: set it in the ini
    // OptiScaler wrote, after the steps that create it. Below 1.0 it lowers the
    // neural pass's frame cost (0.75 ≈ half, 0.5 ≈ a quarter).
    if engine == Engine::Opti {
        if let Some(scale) = x.model_scale {
            let ini = st.game_dir().join(OPTI_INI);
            if let Ok(text) = fs::read_to_string(&ini) {
                if let Some(patched) = scale_ini(&text, scale) {
                    fs::write(&ini, patched)?;
                    results.push((
                        "OptiScaler model resolution".to_owned(),
                        vec![format!(
                            "model runs at {:.0}% of native ([DlssNr] WorkingScale={scale:.2})",
                            scale * 100.0
                        )],
                    ));
                }
            }
        }
    }
    // Re-inspect and refuse a hollow "success" when critical files are missing.
    st = game::inspect(exe)?;
    let missing = missing_install_files(&st);
    if !missing.is_empty() {
        bail!(
            "Install finished but files are missing: {}. Not reporting success.",
            missing.join(", ")
        );
    }
    Ok(results)
}

/// Convenience wrapper used by CLI / GUI when no explicit quality is passed —
/// reads `%LOCALAPPDATA%\dlss5oneclick\settings.json` for defaults.
pub fn run_all(
    exe: &Path,
    engine: Engine,
    x: Extras,
    progress: Progress,
    step_cb: &(dyn Fn(usize, usize, &str, StepState, &str) + Sync),
) -> Result<Vec<(String, Vec<String>)>> {
    let s = crate::settings::Settings::load();
    run_all_with(
        exe,
        engine,
        x,
        InstallOpts {
            quality: s.quality_choice(),
            overrides: s.quality_overrides(),
        },
        progress,
        step_cb,
    )
}

/// Remove everything this tool places except ReShade itself and nvngx_dlss.dll.
pub fn uninstall(exe: &Path) -> Result<Vec<String>> {
    let d = exe.parent().context("exe has no parent")?;
    // A Remix game has none of the ReShade/feeder layout: its install is the
    // model and marker inside `.trex/`, the rtx.conf line, and any runtime swap.
    if let Some(trex) = crate::remix::find_runtime(d) {
        return uninstall_remix(&trex);
    }
    let shaders = game::join_ci(d, &["reshade-shaders", "Shaders"]);
    let include = game::join_ci(&shaders, &["include"]);
    let mut targets: Vec<PathBuf> = vec![
        game::join_ci(d, &[game::DLSS_MARKER]),
        game::join_ci(d, &[game::DLSSNR_MARKER]),
        game::join_ci(d, &[game::FEEDER_MARKER]),
        game::join_ci(d, &[game::FEEDER_ADDON]),
        game::join_ci(d, &[game::DLSS5_ADDON]),
        game::join_ci(d, &[game::DLSS5_ADDON_MARKER]),
        game::join_ci(d, &[game::DLSSNR_DLL]),
        game::join_ci(d, &[game::BRIDGE_ADDON]),
        game::join_ci(d, &[game::UPSTREAM_ADDON]),
        game::join_ci(d, &[game::MFG_ADDON]),
        game::join_ci(d, &["dlss5-dx11-bridge.addon64"]),
        game::join_ci(&shaders, &[game::FEEDER_FX]),
        game::join_ci(d, &["reshade-shaders", "Textures", game::LUMENITE_BLUENOISE]),
    ];
    targets.extend(
        game::RESHADE_HEADERS
            .iter()
            .map(|h| game::join_ci(&shaders, &[h])),
    );
    for (dir, ext) in [(&shaders, "fx"), (&include, "fxh")] {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.starts_with("lumenite_") && name.ends_with(&format!(".{ext}")) {
                    targets.push(e.path());
                }
            }
        }
    }
    if game::join_ci(d, &[game::DLSS_MARKER]).is_file() {
        targets.push(game::join_ci(d, &[game::DLSS_DLL]));
    }
    // The frame-generation provider: ours goes, and the game's own comes back
    // from .original if we moved it aside (#90). The restore happens below,
    // once `removed` exists, so it can be reported.
    // dgVoodoo: only a copy this tool downloaded goes, and its conf only when
    // this tool created it rather than merging into the user's own. Leaving it
    // behind meant a DX9 game that would not start still would not start after
    // Remove, with nothing naming the file responsible (#91).
    if let Ok(m) = fs::read_to_string(d.join(game::DGVOODOO_MARKER)) {
        targets.push(d.join("d3d9.dll"));
        targets.push(d.join(game::DGVOODOO_MARKER));
        if m.lines().any(|l| l.trim() == "conf-ours") {
            targets.push(d.join(game::DGVOODOO_CONF));
        }
    }
    let restore_dlssg = d.join(game::DLSSG_MARKER).is_file();
    if restore_dlssg {
        targets.push(d.join(game::DLSSG_MARKER));
        if !d.join(game::DLSSG_BACKUP).is_file() {
            targets.push(d.join(game::DLSSG_DLL));
        }
    }
    // 32-bit layout: the in-game addon32 and everything in host64\.
    targets.push(game::join_ci(d, &[game::FEEDER_ADDON32]));
    let host = game::join_ci(d, &[game::HOST_DIR]);
    if host.is_dir() {
        for f in [
            game::HOST_EXE,
            game::DLSS5_ADDON,
            game::DLSS5_ADDON_MARKER,
            game::DLSSNR_DLL,
            game::DLSSNR_MARKER,
            game::DLSS_MARKER,
        ] {
            targets.push(host.join(f));
        }
        if host.join(game::DLSS_MARKER).is_file() {
            targets.push(host.join(game::DLSS_DLL));
        }
        if host.join(game::RESHADE_MARKER).is_file() {
            targets.push(host.join(game::RESHADE_PROXY));
            targets.push(host.join(game::RESHADE_MARKER));
        }
        // The two .log files are evidence, not installed files. Remove used to
        // delete them, which erased the very verdict the next Install reads to
        // decide which add-on build this machine can use (#69).
        for n in ["ReShade.ini", "ReShadePreset.ini"] {
            targets.push(host.join(n));
        }
    }
    if let Ok(name) = fs::read_to_string(d.join(game::RENODX_MANIFEST)) {
        let name = name.trim();
        if name.starts_with("renodx-") && !name.contains(['/', '\\']) {
            targets.push(d.join(name));
        }
        targets.push(d.join(game::RENODX_MANIFEST));
    }
    if d.join(game::REFRAMEWORK_MARKER).is_file() {
        targets.push(d.join(game::REFRAMEWORK_DLL));
        targets.push(d.join(game::REFRAMEWORK_MARKER));
    }
    let mut removed = Vec::new();
    uninstall_opti(d, &mut removed)?;
    crate::mfg::uninstall(d, &mut removed)?;
    for t in targets {
        if t.is_file() {
            fs::remove_file(&t)?;
            removed.push(
                t.strip_prefix(d)
                    .unwrap_or(&t)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    if restore_dlssg {
        let backup = d.join(game::DLSSG_BACKUP);
        let dll = d.join(game::DLSSG_DLL);
        if backup.is_file() {
            let _ = fs::remove_file(&dll);
            if fs::rename(&backup, &dll).is_ok() {
                removed.push(format!("{} (the game's own restored)", game::DLSSG_DLL));
            }
        }
    }
    if include.is_dir() && fs::read_dir(&include)?.next().is_none() {
        fs::remove_dir(&include)?;
    }
    // The Windows GPU preference this tool wrote goes too, but only when it is
    // still exactly what was written (a user's own choice is left alone).
    for e in [
        exe.to_path_buf(),
        d.join(game::HOST_DIR).join(game::HOST_EXE),
    ] {
        if gpupref::clear_ours(&e).unwrap_or(false) {
            removed.push(format!(
                "Windows GPU preference for {}",
                e.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    let host = d.join(game::HOST_DIR);
    if host.is_dir() && fs::read_dir(&host)?.next().is_none() {
        fs::remove_dir(&host)?;
        removed.push(format!("{}/", game::HOST_DIR));
    }
    Ok(removed)
}

/// `uninstall`, then ReShade itself (`dxgi.dll` + ini/logs).
///
/// Refuses only when a foreign `.addon64`/`.addon32` remains — those need
/// ReShade to load. Leftover shaders under `reshade-shaders` (common on older
/// packs, e.g. Gothic 3) no longer block removal: this tool always installs
/// ReShade as `dxgi.dll`, never as `d3d9.dll`, and never deletes dgVoodoo's
/// `d3d9.dll` / `dgVoodoo.conf`. `dxgi.dll` is only deleted when it
/// verifiably is a ReShade DLL. Returns `(removed, kept_reason)`;
/// `kept_reason` is `Some` when ReShade was left.
pub fn uninstall_all(exe: &Path) -> Result<(Vec<String>, Option<String>)> {
    let mut removed = uninstall(exe)?;
    let d = exe.parent().context("exe has no parent")?;
    // A Remix game has no ReShade to also remove; uninstall() already did it all.
    if crate::remix::find_runtime(d).is_some() {
        return Ok((removed, None));
    }

    let mut foreign_addons: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".addon64") || n.ends_with(".addon32") {
                foreign_addons.push(n);
            }
        }
    }
    if !foreign_addons.is_empty() {
        foreign_addons.sort();
        foreign_addons.truncate(6);
        return Ok((
            removed,
            Some(format!(
                "ReShade left in place: the game still has add-ons this tool did not install ({})",
                foreign_addons.join(", ")
            )),
        ));
    }

    let mut rm = |p: PathBuf| -> Result<()> {
        if p.is_file() {
            fs::remove_file(&p)?;
            removed.push(
                p.strip_prefix(d)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
        Ok(())
    };
    let proxy = game::join_ci(d, &[game::RESHADE_PROXY]);
    if game::is_reshade_dll(&proxy) {
        rm(proxy)?;
    }
    rm(d.join(game::RESHADE_MARKER))?;
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            let reshade_file = (n.starts_with("reshade")
                && (n.ends_with(".ini") || n.ends_with(".log")))
                || n.starts_with("reshadepreset")
                || n.starts_with("dlss5-feed.");
            if reshade_file {
                rm(e.path())?;
            }
        }
    }
    let shaders_root = d.join("reshade-shaders");
    let mut leftover_shaders = false;
    let mut walk = vec![shaders_root.clone()];
    while let Some(dir) = walk.pop() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk.push(p);
                } else {
                    leftover_shaders = true;
                    break;
                }
            }
        }
        if leftover_shaders {
            break;
        }
    }
    if shaders_root.is_dir() {
        if leftover_shaders {
            removed.push("reshade-shaders/ (left: shaders this tool did not install)".into());
        } else {
            fs::remove_dir_all(&shaders_root)?;
            removed.push("reshade-shaders/".into());
        }
    }
    Ok((removed, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::testutil::*;
    use serde_json::json;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn rhi_releases() -> Vec<Value> {
        ["streamline-2.13.0.0", "renodx-dlss5-4.55", "renodx-dlss5-4.5", "renodx-dlss5-3.3.4",
         "dlssnr-310.8.SF-v2", "dlssnr-310.8.SF", "dlssg-310.8.0", "dlssd-310.7.129",
         "dlss-310.8.0", "dlss-310.7.129", "DLSS-Enabler-4.9.0.7"]
            .iter()
            .map(|t| json!({"tag_name": t, "assets": [{"browser_download_url": format!("https://x/{t}.zip")}]}))
            .collect()
    }

    /// Both OptiScaler forks publish a rolling "nightly" release whose assets
    /// are .7z, and the stable ones ship a checksum .txt beside the zip. Taking
    /// a release's first asset picked whichever happened to be listed first (#72).
    #[test]
    fn opti_zip_is_picked_over_checksums_and_7z() {
        let releases = json!([
            {"prerelease": false, "tag_name": "nightly", "assets": [
                {"name": "OptiScaler_v10.0.0-pre1_20260908.7z", "browser_download_url": "https://x/n.7z"}
            ]},
            {"prerelease": false, "tag_name": "v0.7.1-hybrid", "assets": [
                {"name": "ASSET-SHA256SUMS-v0.7.1.txt", "browser_download_url": "https://x/sums.txt"},
                {"name": "OptiScaler-DLSSNR-v0.7.1-hybrid.zip", "browser_download_url": "https://x/good.zip"}
            ]}
        ]);
        assert_eq!(
            pick_opti_zip(releases.as_array().unwrap()).as_deref(),
            Some("https://x/good.zip")
        );
    }

    /// The engine choice decides which fork is fetched, and nothing else. It
    /// reaches the step through the install's own context, never the process
    /// environment, so it holds only on the thread and for the run it was set.
    #[test]
    fn opti_source_selects_the_fork() {
        assert_eq!(opti_repo_for(false), OPTI_REPO);
        assert_eq!(opti_repo_for(true), OPTI_PRESR_REPO);
        assert_eq!(opti_repo(), OPTI_REPO, "outside an install nothing asked for the fork");
        {
            let _run = InstallScope::enter(
                quality_preset::fallback_medium(),
                Extras {
                    opti_presr: true,
                    ..Default::default()
                },
            );
            assert_eq!(opti_repo(), OPTI_PRESR_REPO);
            let elsewhere = std::thread::spawn(opti_repo).join().unwrap();
            assert_eq!(elsewhere, OPTI_REPO, "another thread's install is not this one");
        }
        assert_eq!(opti_repo(), OPTI_REPO, "and the choice ends with the run");
    }

    /// The neural-upstream preset comes from the install, and only a real one
    /// is written; anything else leaves the overlay's own.
    #[test]
    fn upstream_preset_comes_from_the_install_and_must_be_real() {
        let (_, real, _) = reshade_ini::UPSTREAM_PRESETS[0];
        let during = |p: u8| {
            let _run = InstallScope::enter(
                quality_preset::fallback_medium(),
                Extras {
                    upstream: true,
                    upstream_preset: p,
                    ..Default::default()
                },
            );
            upstream_preset()
        };
        assert_eq!(during(real), real);
        assert_eq!(during(250), 0);
        assert_eq!(upstream_preset(), 0);
    }

    /// Dying Light refuses any reduced work resolution: identical failure at
    /// 90% and 85%, fine at 100%. Re-running Install used to write the preset's
    /// smaller value straight back and break the game again (#74).
    #[test]
    fn a_game_that_refused_a_smaller_work_resolution_keeps_full_size() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let mut q = quality_preset::fallback_medium();
        q.work_resolution = 85;
        q.work_upscale = 1;

        // No log yet: the preset is written as chosen.
        write_feeder_cfg(d, &q).unwrap();
        let cfg = fs::read_to_string(d.join("dlss5-feed.cfg")).unwrap();
        assert!(cfg.contains("work_resolution=85"), "{cfg}");

        // A log carrying the failure pins it back to full size.
        fs::write(
            d.join("dlss5-feed.log"),
            "[feed] building: 2304x1296 work resolution (90%) -> 2560x1440 backbuffer\n\
             [feed] work-resolution staging SRV failed\n\
             [feed] failure: resource build\n",
        )
        .unwrap();
        assert!(work_resolution_refused(d));
        write_feeder_cfg(d, &q).unwrap();
        let cfg = fs::read_to_string(d.join("dlss5-feed.cfg")).unwrap();
        assert!(cfg.contains("work_resolution=100"), "{cfg}");
        assert!(cfg.contains("work_upscale=0"), "{cfg}");
    }

    /// Reinstalling or updating lays the preset over an existing cfg instead
    /// of replacing it: a raised create_delay and the user's comments survive,
    /// the preset's keys move, and the Feeder is told to profile again.
    #[test]
    fn write_feeder_cfg_keeps_what_the_preset_does_not_decide() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let mut q = quality_preset::fallback_medium();
        q.work_resolution = 90;
        fs::write(
            d.join("dlss5-feed.cfg"),
            "; tuned for this game\ncreate_delay=240\nwork_resolution=70\nauto_profile_applied=1\n",
        )
        .unwrap();
        write_feeder_cfg(d, &q).unwrap();
        let cfg = fs::read_to_string(d.join("dlss5-feed.cfg")).unwrap();
        assert!(cfg.contains("; tuned for this game\ncreate_delay=240\n"), "{cfg}");
        assert!(cfg.contains("work_resolution=90\n"), "{cfg}");
        assert!(cfg.contains("auto_profile_applied=0\n"), "{cfg}");
    }

    /// A machine whose own log carries the driver-fault verdict must not be
    /// handed the same add-on build again. The evidence has to come from the
    /// log, not from a driver number, because the measurement upstream covers
    /// one driver and assumes the rest (#69).
    /// On a 32-bit game the host writes that verdict into host64\ while the
    /// feed's own log sits beside the exe. Reading only one folder missed it,
    /// and the exclusion of 32-bit games on top of that meant GTA IV was handed
    /// the faulting build every single install (#69).
    #[test]
    fn the_driver_verdict_is_found_from_either_side_of_a_32_bit_layout() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path();
        let host = game.join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        let flag = t.path().join("state").join("driver-fault.txt");
        let faulted = |dir: &Path| faulted_in_driver_at(dir, &flag, Some("616.64".to_owned()));
        assert!(!faulted(&host));

        // The host's own log, which is where a 32-bit game records it.
        fs::write(
            host.join("dlss5-feed-host.log"),
            "[host] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted(&host));

        // And the feed's log beside the exe, one level up from the consumer dir.
        let t2 = tempfile::tempdir().unwrap();
        let host2 = t2.path().join(game::HOST_DIR);
        fs::create_dir_all(&host2).unwrap();
        fs::write(
            t2.path().join("dlss5-feed.log"),
            "[feed] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted(&host2));
    }

    /// Remove deletes the game folder's logs, and a fresh game has none yet, so
    /// a verdict read out of a log has to outlive the folder it was read in.
    /// sempie27 was told to Remove and Install, which erased the evidence and
    /// handed him the faulting build again (#69).
    #[test]
    fn the_driver_verdict_outlives_the_folder_it_was_read_in() {
        let t = tempfile::tempdir().unwrap();
        let host = t.path().join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();
        let flag = t.path().join("state").join("driver-fault.txt");
        let drv = || Some("616.64".to_owned());

        assert!(!faulted_in_driver_at(&host, &flag, drv()));

        fs::write(
            host.join("dlss5-feed-host.log"),
            "[host] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted_in_driver_at(&host, &flag, drv()));
        assert_eq!(fs::read_to_string(&flag).unwrap().trim(), "616.64");

        // Remove empties the folder; the verdict survives it.
        fs::remove_file(host.join("dlss5-feed-host.log")).unwrap();
        assert!(
            faulted_in_driver_at(&host, &flag, drv()),
            "the verdict must outlive the log"
        );

        // A driver update retires it, and the stale flag is dropped.
        assert!(!faulted_in_driver_at(
            &host,
            &flag,
            Some("620.10".to_owned())
        ));
        assert!(!flag.is_file());
    }

    #[test]
    fn a_driver_fault_in_the_log_pins_the_classic_addon() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let flag = t.path().join("state").join("driver-fault.txt");
        let faulted = |dir: &Path| faulted_in_driver_at(dir, &flag, Some("616.64".to_owned()));
        assert!(!faulted(d));

        fs::write(
            d.join("dlss5-feed-host.log"),
            "[host] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail (on 616.64 exactly; anything newer is untested here). The neural \
             evaluate faults inside the driver's own NGX runtime -- an access violation in \
             D3D12Core.dll, reached through nvngx_dlssnr.dll\n",
        )
        .unwrap();
        assert!(faulted(d));

        // The feed's own log carries the same verdict on a 64-bit game.
        let d2 = tempfile::tempdir().unwrap();
        fs::write(
            d2.path().join("dlss5-feed.log"),
            "[feed] WARNING: renodx-dlss5 v4.7 with NVIDIA driver 616.64 is a combination \
             measured to fail\n",
        )
        .unwrap();
        assert!(faulted(d2.path()));

        // A healthy log changes nothing.
        let d3 = tempfile::tempdir().unwrap();
        fs::write(d3.path().join("dlss5-feed.log"), "[feed] feature ready\n").unwrap();
        let fresh = t.path().join("state2").join("driver-fault.txt");
        assert!(!faulted_in_driver_at(
            d3.path(),
            &fresh,
            Some("616.64".to_owned())
        ));
    }

    /// Two neural consumers in one folder is not two implementations to choose
    /// from: both detour the same NGX entry points and create feature 18 on the
    /// same device, the second create is refused (0xBAD0000B), and the user gets
    /// neither. Installing once with the default and again with Neural Upstream
    /// left exactly that (#75).
    #[test]
    fn the_upstream_route_removes_the_addon_it_replaces() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        fs::write(t.path().join(game::DLSS_DLL), b"x").unwrap(); // native DLSS
        let addon = t.path().join(game::DLSS5_ADDON);
        fs::write(&addon, b"addon").unwrap();
        let st = game::inspect(&exe).unwrap();

        // The step is in the plan for that route, and not for the default one.
        let named = |v: Vec<Step>| -> Vec<&'static str> { v.iter().map(|s| s.name).collect() };
        let with = named(plan_with(&st, Engine::ReShade, Extras { upstream: true, ..Default::default() }));
        let without = named(plan_with(&st, Engine::ReShade, Extras::default()));
        assert!(
            with.iter().any(|n| n.contains("Remove the RenoDX")),
            "{with:?}"
        );
        assert!(
            !without.iter().any(|n| n.contains("Remove the RenoDX")),
            "{without:?}"
        );

        // And it takes the file out.
        let c = reqwest::blocking::Client::new();
        step_dlss5_cleanup(&c, &st, t.path(), &|_, _| {}).unwrap();
        assert!(!addon.exists());
        // A second run is a no-op rather than an error.
        step_dlss5_cleanup(&c, &st, t.path(), &|_, _| {}).unwrap();
    }

    /// Feeder 0.15.0 fixed the sRGB staging-view bug that made every reduced
    /// work resolution fail. A failure logged by an older build must stop
    /// holding the setting down, or the fix never reaches anyone who hit it
    /// (DLSS5-Feeder#85).
    #[test]
    fn the_work_resolution_hold_expires_with_the_feeder_that_logged_it() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let log = |ver: &str| {
            fs::write(
                d.join("dlss5-feed.log"),
                format!(
                    "02:08:22.172  dlss5-feed {ver} (built Sep  7 2026 07:47:43) attached.\n\
                     [feed] work-resolution staging SRV failed\n"
                ),
            )
            .unwrap();
        };

        log("0.14.0-beta.5");
        assert!(
            !work_resolution_refused(d),
            "an old build's failure is stale"
        );
        log("0.15.0");
        assert!(
            work_resolution_refused(d),
            "the fixed build still failing counts"
        );
        log("0.16.2");
        assert!(work_resolution_refused(d));

        // A log with no version line at all is still taken at its word.
        fs::write(
            d.join("dlss5-feed.log"),
            "[feed] work-resolution staging SRV failed\n",
        )
        .unwrap();
        assert!(work_resolution_refused(d));
    }

    /// The 32-bit halves talk a versioned protocol; if one is refreshed and the
    /// other is not, the host exits at startup and nothing says why from inside
    /// the game. Same size is not the same release (#69).
    #[test]
    fn both_thirty_two_bit_halves_carry_the_tag_they_came_from() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let host = d.join(game::HOST_DIR);
        fs::create_dir_all(&host).unwrap();

        // What an install writes.
        fs::write(d.join(game::FEEDER_MARKER), b"v0.15.1").unwrap();
        fs::write(host.join(game::FEEDER_MARKER), b"v0.15.1").unwrap();
        let agree = |tag: &str| {
            let says = |dir: &Path| {
                fs::read_to_string(dir.join(game::FEEDER_MARKER)).is_ok_and(|m| m.trim() == tag)
            };
            says(d) && says(&host)
        };
        assert!(agree("v0.15.1"));

        // A newer release: neither half is current, so both are replaced.
        assert!(!agree("v0.16.0"));

        // The failure this fixes: the helper refreshed, the in-game half not.
        fs::write(host.join(game::FEEDER_MARKER), b"v0.16.0").unwrap();
        assert!(
            !agree("v0.16.0"),
            "a half-updated pair must not look current"
        );
    }

    /// RTX 40 MFG is one ini key and no extra files, so it can be offered as
    /// part of an install. The Ampere/Turing key in the same section sideloads
    /// a DLL with no published release and is deliberately never written (#83).
    /// Remove left dgVoodoo's d3d9.dll in every DX9 game it had been installed
    /// into, so a game that would not start still would not start afterwards,
    /// and nothing said which file to delete (#91). Only a copy this tool
    /// downloaded goes, and the conf only when this tool created it.
    #[test]
    fn removing_takes_our_dgvoodoo_out_and_leaves_a_user_s_alone() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);

        // Ours, conf included.
        fs::write(d.join("d3d9.dll"), b"dgVoodoo").unwrap();
        fs::write(d.join(game::DGVOODOO_CONF), b"[General]\n").unwrap();
        fs::write(
            d.join(game::DGVOODOO_MARKER),
            format!("{DGVOODOO_TAG}\nconf-ours\n"),
        )
        .unwrap();
        uninstall(&exe).unwrap();
        assert!(!d.join("d3d9.dll").exists());
        assert!(!d.join(game::DGVOODOO_CONF).exists());
        assert!(!d.join(game::DGVOODOO_MARKER).exists());

        // Ours, but the conf was the user's before we merged into it.
        fs::write(d.join("d3d9.dll"), b"dgVoodoo").unwrap();
        fs::write(d.join(game::DGVOODOO_CONF), b"[General]\n").unwrap();
        fs::write(d.join(game::DGVOODOO_MARKER), format!("{DGVOODOO_TAG}\n")).unwrap();
        uninstall(&exe).unwrap();
        assert!(!d.join("d3d9.dll").exists());
        assert!(d.join(game::DGVOODOO_CONF).is_file(), "their conf stays");

        // Someone else's d3d9.dll, no marker: untouched.
        fs::write(d.join("d3d9.dll"), b"theirs").unwrap();
        uninstall(&exe).unwrap();
        assert_eq!(fs::read(d.join("d3d9.dll")).unwrap(), b"theirs");
    }

    /// The MFG add-on validates the frame-generation provider by build and
    /// refuses anything else, so ours goes in and the game's own is kept as
    /// .original — Remove has to put that back, not delete it (#90).
    #[test]
    fn removing_the_mfg_provider_restores_the_game_s_own() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        fs::write(d.join(game::DLSSG_DLL), b"ours").unwrap();
        fs::write(d.join(game::DLSSG_BACKUP), b"the game's").unwrap();
        fs::write(d.join(game::DLSSG_MARKER), b"dlssg-310.9.1").unwrap();

        uninstall(&exe).unwrap();
        assert_eq!(
            fs::read(d.join(game::DLSSG_DLL)).unwrap(),
            b"the game's",
            "the game's provider must come back"
        );
        assert!(!d.join(game::DLSSG_BACKUP).exists());
        assert!(!d.join(game::DLSSG_MARKER).exists());

        // With no backup, ours is simply removed.
        fs::write(d.join(game::DLSSG_DLL), b"ours").unwrap();
        fs::write(d.join(game::DLSSG_MARKER), b"dlssg-310.9.1").unwrap();
        uninstall(&exe).unwrap();
        assert!(!d.join(game::DLSSG_DLL).exists());
    }

    /// The OptiScaler route writes an ini key; the ReShade route needs the
    /// separate add-on, because the fork's built-in unlock reported "DLSSG not
    /// patched: capability not matched" on the reporter's machine (#83). The
    /// add-on is an .addon64, so a 32-bit game never gets it.
    #[test]
    fn mfg_addon_is_planned_on_the_reshade_route_only() {
        let mut st = game::stub_status(game::Mode::Native, game::Api::Dx12);
        st.gpu = Some((rtx("RTX 4070"), crate::gpu::Tier::Rtx40));
        st.has_fg = true;
        let named = |v: &[Step]| -> Vec<&'static str> { v.iter().map(|s| s.name).collect() };
        let mfg = Extras {
            ada_mfg: true,
            ..Default::default()
        };

        assert!(!named(&plan_with(&st, Engine::ReShade, Extras::default())).contains(&STEP_MFG.name));

        let reshade = named(&plan_with(&st, Engine::ReShade, mfg));
        assert!(reshade.contains(&STEP_MFG.name), "{reshade:?}");
        // Ahead of ReShade config, which writes the add-on list.
        let at = reshade.iter().position(|n| *n == STEP_MFG.name).unwrap();
        let cfg = reshade.iter().position(|n| *n == STEP_CONFIG.name).unwrap();
        assert!(at < cfg, "{reshade:?}");
        // The OptiScaler route has its own ini key and must not fetch it.
        assert!(!named(&plan_with(&st, Engine::Opti, mfg)).contains(&STEP_MFG.name));
    }

    fn rtx(name: &str) -> crate::gpu::Gpu {
        crate::gpu::Gpu {
            name: name.into(),
            vendor: "NVIDIA".into(),
        }
    }

    /// Multi-frame generation is offered where it can do something: an RTX 40,
    /// a game with frame generation of its own, a 64-bit ReShade route or the
    /// pre-SR OptiScaler build. Planned, it takes the older dashdogy unlock out
    /// first, since the two must never run together.
    #[test]
    fn mfg_is_offered_only_where_it_can_work() {
        use crate::gpu::Tier;
        let mut st = game::stub_status(game::Mode::Native, game::Api::Dx12);
        st.gpu = Some((rtx("RTX 4090"), Tier::Rtx40));
        assert!(mfg_unavailable(&st, Engine::ReShade, false).is_some(), "no FG of its own");
        st.has_fg = true;
        assert_eq!(mfg_unavailable(&st, Engine::ReShade, false), None);
        assert!(mfg_unavailable(&st, Engine::Opti, false).is_some(), "stable OptiScaler has none");
        assert_eq!(mfg_unavailable(&st, Engine::Opti, true), None);
        st.bitness = 32;
        assert!(mfg_unavailable(&st, Engine::ReShade, false).is_some(), "the add-on is 64-bit");
        st.bitness = 64;
        st.gpu = Some((rtx("RTX 5090"), Tier::Rtx50));
        assert!(mfg_unavailable(&st, Engine::ReShade, false).is_some());
        let x = Extras {
            ada_mfg: true,
            ..Default::default()
        };
        assert!(!plan_with(&st, Engine::ReShade, x).iter().any(|s| s.name == STEP_MFG.name));

        st.gpu = Some((rtx("RTX 4090"), Tier::Rtx40));
        st.mfg_asi = true;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, x).iter().map(|s| s.name).collect();
        let cleanup = names.iter().position(|n| *n == STEP_MFG_ASI_CLEANUP.name);
        let mfg = names.iter().position(|n| *n == STEP_MFG.name);
        assert!(cleanup.is_some() && mfg.is_some() && cleanup < mfg, "{names:?}");
    }

    /// The pre-SR build's unlock lives in OptiScaler.ini; the tick reads it back.
    #[test]
    fn opti_ada_mfg_reads_the_framegen_key() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert!(!opti_ada_mfg(d));
        fs::write(d.join(OPTI_INI), "[FrameGen]\nAdaMfgUnlock=false\n").unwrap();
        assert!(!opti_ada_mfg(d));
        fs::write(d.join(OPTI_INI), "[FrameGen]\nAdaMfgUnlock=True\n").unwrap();
        assert!(opti_ada_mfg(d));
    }

    #[test]
    fn ada_mfg_is_written_and_ampere_is_left_alone() {
        assert_eq!(ada_mfg(), "false");
        {
            let _run = InstallScope::enter(
                quality_preset::fallback_medium(),
                Extras {
                    ada_mfg: true,
                    ..Default::default()
                },
            );
            assert_eq!(ada_mfg(), "true");
        }
        assert_eq!(ada_mfg(), "false");

        let ini = "[FrameGen]\nAdaMfgUnlock=false\nAmpereMfgUnlock=false\n";
        let out = set_ini_key(ini, "FrameGen", "AdaMfgUnlock", "true").unwrap();
        assert!(out.contains("AdaMfgUnlock=true"), "{out}");
        // The two are mutually exclusive upstream: "Never combine with
        // AdaMfgUnlock or an external MFG unlocker."
        assert!(out.contains("AmpereMfgUnlock=false"), "{out}");
    }

    #[test]
    fn fg_ini_sets_the_framegen_keys_and_is_idempotent() {
        // A real OptiScaler.ini has these sections with auto values; only the
        // FrameGen/OptiFG ones must move, never another section's Enabled.
        let ini = "[FrameGen]\nEnabled=auto\nFGInput=auto\nFGOutput=auto\n\n\
                   [OptiFG]\nHUDFix=auto\n\n[DlssNr]\nEnabled=true\n";
        let (out, set) = fg_ini(ini);
        assert!(out.contains("[FrameGen]"));
        assert!(out.contains("Enabled=true\nFGInput=upscaler\nFGOutput=fsrfg"));
        assert!(out.contains("[OptiFG]\nHUDFix=true"));
        // DlssNr's own Enabled=true is untouched (section-scoped).
        assert!(out.contains("[DlssNr]\nEnabled=true"));
        assert_eq!(set.len(), 4);
        // Running it again changes nothing.
        let (_, again) = fg_ini(&out);
        assert!(again.is_empty());
    }

    /// The dial opens on what the game's ini already says. `auto`, a missing
    /// key, a commented-out one and nonsense all read as "not set", and another
    /// section's identically-named key is not mistaken for it.
    #[test]
    fn opti_working_scale_reads_the_dlssnr_value() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert_eq!(opti_working_scale(d), None);
        let ini = d.join(OPTI_INI);
        fs::write(&ini, "[FrameGen]\nWorkingScale=0.5\n\n[DlssNr]\nWorkingScale=auto\n").unwrap();
        assert_eq!(opti_working_scale(d), None);
        fs::write(&ini, "[DlssNr]\n;WorkingScale=0.5\nWorkingScale = 0.75\n").unwrap();
        assert_eq!(opti_working_scale(d), Some(0.75));
        fs::write(&ini, "[DlssNr]\nWorkingScale=9\n").unwrap();
        assert_eq!(opti_working_scale(d), None);
    }

    #[test]
    fn scale_ini_sets_workingscale_section_scoped() {
        let ini = "[FrameGen]\nWorkingScale=99\n\n[DlssNr]\nEnabled=true\nWorkingScale=auto\n";
        let out = scale_ini(ini, 0.75).unwrap();
        // Only DlssNr's WorkingScale moves, not FrameGen's identically-named key.
        assert!(out.contains("[DlssNr]\nEnabled=true\nWorkingScale=0.75"));
        assert!(out.contains("[FrameGen]\nWorkingScale=99"));
        // Idempotent.
        assert!(scale_ini(&out, 0.75).is_none());
        // Half formats cleanly.
        assert!(scale_ini(ini, 0.5).unwrap().contains("WorkingScale=0.50"));
    }

    #[test]
    fn opti_plan_adds_frame_generation_after_optiscaler() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let mut st = game::inspect(&exe).unwrap();
        st.mode = game::Mode::Native;
        st.api = game::Api::Dx12;
        let names: Vec<&str> = plan_with(
            &st,
            Engine::Opti,
            Extras {
                with_fg: true,
                ..Default::default()
            },
        )
        .iter()
        .map(|s| s.name)
        .collect();
        let opti = names.iter().position(|n| *n == STEP_OPTI.name).unwrap();
        let fg = names.iter().position(|n| *n == STEP_OPTI_FG.name).unwrap();
        assert!(fg == opti + 1, "FG must run right after OptiScaler: {names:?}");
        // Off by default.
        let off: Vec<&str> = plan_with(&st, Engine::Opti, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert!(!off.contains(&STEP_OPTI_FG.name));
    }

    #[test]
    fn remix_mod_extract_and_remove_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        // A game file the mod will replace, so backup/restore is exercised.
        std::fs::write(game.join("d3d9.dll"), b"vanilla proxy").unwrap();

        // Build a fixture mod zip: the runtime sits one folder down.
        let zip_path = t.path().join("mod.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opt = SimpleFileOptions::default();
            for (name, body) in [
                ("GTAIV-Remix/.trex/d3d9.dll", &b"remix runtime"[..]),
                ("GTAIV-Remix/.trex/rtx-remix.conf", b"conf"),
                ("GTAIV-Remix/rtx.conf", b"rtx.a = 1"),
                ("GTAIV-Remix/d3d9.dll", b"mod proxy"), // replaces the vanilla one
            ] {
                w.start_file(name, opt).unwrap();
                w.write_all(body).unwrap();
            }
            w.finish().unwrap();
        }

        let placed = place_remix_mod(&zip_path, &game, "xoxor4d/gta4-rtx v1.5.2").unwrap();
        assert!(placed[0].contains("4 files"));
        // The runtime landed and the game is now a Remix game.
        assert!(game.join(".trex").join("d3d9.dll").is_file());
        assert!(crate::remix::find_runtime(&game).is_some());
        // The vanilla file was backed up, the mod's copy is in place.
        assert_eq!(std::fs::read(game.join("d3d9.dll")).unwrap(), b"mod proxy");
        assert!(game
            .join(format!("d3d9.dll{REMIX_MOD_ORIG}"))
            .is_file());
        assert!(game.join(REMIX_MOD_MANIFEST).is_file());

        // Remove: files gone, the vanilla proxy restored, folders cleaned.
        uninstall_remix_mod(&game).unwrap();
        assert!(!game.join(".trex").exists());
        assert_eq!(std::fs::read(game.join("d3d9.dll")).unwrap(), b"vanilla proxy");
        assert!(!game.join(format!("d3d9.dll{REMIX_MOD_ORIG}")).exists());
        assert!(!game.join(REMIX_MOD_MANIFEST).exists());
    }

    #[test]
    fn uninstall_remix_removes_model_and_reverts_conf_and_swap() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        let trex = d.join(".trex");
        std::fs::create_dir_all(&trex).unwrap();
        std::fs::write(trex.join("d3d9.dll"), b"swapped runtime").unwrap();
        // A backup of the mod's own runtime, as a swap would have left.
        std::fs::write(trex.join(format!("d3d9.dll{REMIX_ORIG}")), b"mod original").unwrap();
        std::fs::write(trex.join(game::DLSSNR_DLL), b"model").unwrap();
        std::fs::write(trex.join(REMIX_MARKER), b"dlssnr-310.8.SF").unwrap();
        std::fs::write(
            d.join("rtx.conf"),
            "rtx.a = 1\nrtx.neuralUplift.enable = True\n",
        )
        .unwrap();

        let removed = uninstall(&exe).unwrap();
        // Model + marker gone; the mod's own runtime restored; enable line gone.
        assert!(!trex.join(game::DLSSNR_DLL).is_file());
        assert!(!trex.join(REMIX_MARKER).is_file());
        assert!(!trex.join(format!("d3d9.dll{REMIX_ORIG}")).is_file());
        assert_eq!(std::fs::read(trex.join("d3d9.dll")).unwrap(), b"mod original");
        let conf = std::fs::read_to_string(d.join("rtx.conf")).unwrap();
        assert_eq!(conf, "rtx.a = 1\n");
        assert!(removed.iter().any(|r| r.contains("nvngx_dlssnr.dll")));
    }

    /// The post-install check judged a Remix install by ReShade/Feeder files
    /// that route never places, so every good Remix install ended in "files
    /// are missing". It is the model in `.trex/` plus the rtx.conf switch.
    #[test]
    fn remix_install_is_judged_by_its_own_files() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        std::fs::create_dir_all(d.join(".trex")).unwrap();
        std::fs::write(d.join(".trex").join("d3d9.dll"), b"stub runtime").unwrap();
        let missing = missing_install_files(&game::inspect(&exe).unwrap());
        assert_eq!(missing.len(), 2, "{missing:?}");
        assert!(!missing.iter().any(|m| m.contains("ReShade")), "{missing:?}");

        std::fs::write(d.join(".trex").join(game::DLSSNR_DLL), b"model").unwrap();
        std::fs::write(d.join("rtx.conf"), "rtx.neuralUplift.enable = True\n").unwrap();
        assert!(missing_install_files(&game::inspect(&exe).unwrap()).is_empty());
    }

    #[test]
    fn remix_game_plans_the_remix_route_regardless_of_engine() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        std::fs::create_dir_all(d.join(".trex")).unwrap();
        std::fs::write(d.join(".trex").join("d3d9.dll"), b"stub runtime").unwrap();
        let st = game::inspect(&exe).unwrap();
        assert!(st.remix.is_some());
        // The engine is ignored: both give the Remix route.
        for engine in [Engine::ReShade, Engine::Opti] {
            let names: Vec<&str> = plan_with(&st, engine, Extras::default())
                .iter()
                .map(|s| s.name)
                .collect();
            assert_eq!(names, ["DLSS 5 into the RTX Remix runtime"]);
        }
        // With the swap on, the runtime is replaced first, then fed.
        let names: Vec<&str> = plan_with(
            &st,
            Engine::ReShade,
            Extras {
                remix_swap: true,
                ..Default::default()
            },
        )
        .iter()
        .map(|s| s.name)
        .collect();
        assert_eq!(
            names,
            [
                "Swap in a DLSS 5-capable Remix runtime",
                "DLSS 5 into the RTX Remix runtime"
            ]
        );
    }

    #[test]
    fn feeder_key_orders_betas_below_the_release() {
        assert!(feeder_key("v0.7.0") < feeder_key("v0.8.0-beta.3"));
        assert!(feeder_key("v0.8.0-beta.3") < feeder_key("v0.8.0"));
        assert!(feeder_key("v0.8.0-beta.1") < feeder_key("v0.8.0-beta.3"));
        assert!(feeder_key("v0.9.0-beta.1") > feeder_key("v0.8.0"));
    }

    #[test]
    fn an_old_feeder_needs_the_classic_addon() {
        // The stable feeder line (< 0.8.0-beta.3) speaks only 4.55.
        assert!(feeder_needs_classic(Some("v0.7.0")));
        assert!(feeder_needs_classic(Some("v0.8.0-beta.1")));
        // One that knows the newer add-ons holds nothing back, and neither does
        // the native route, which has no feeder at all.
        assert!(!feeder_needs_classic(Some("v0.8.0-beta.3")));
        assert!(!feeder_needs_classic(Some("v0.9.0-beta.1")));
        assert!(!feeder_needs_classic(None));
        // The line held to is the classic build upstream names.
        assert_eq!(RENODX_CLASSIC_TAG, format!("renodx-dlss5-{CLASSIC_LINE}"));
    }

    #[test]
    fn label_is_matches_the_pinned_line_only() {
        assert!(label_is("renodx-dlss5-4.55", "renodx-dlss5-", "4.55"));
        assert!(label_is("renodx-dlss5-4.55.1", "renodx-dlss5-", "4.55")); // same line
        assert!(!label_is("renodx-dlss5-4.5", "renodx-dlss5-", "4.55"));
        assert!(!label_is("renodx-dlss5-4.7", "renodx-dlss5-", "4.55"));
        // The pinned filter over a release list selects exactly 4.55.
        let r = rhi_releases();
        let hit: Vec<&str> = r
            .iter()
            .filter_map(|v| v.get("tag_name")?.as_str())
            .filter(|t| label_is(t, "renodx-dlss5-", "4.55"))
            .collect();
        assert_eq!(hit, vec!["renodx-dlss5-4.55"]);
    }

    #[test]
    fn dlssnr_prefers_multi_generation_sf_build() {
        let r: Vec<Value> = ["dlssnr-310.8.0", "dlssnr-310.8.0-RTX40", "dlssnr-310.8.SF", "dlssnr-310.8.SF-v2", "dlssnr-310.9.0"]
            .iter()
            .map(|t| json!({"tag_name": t, "assets": [{"browser_download_url": format!("https://x/{t}.zip")}]}))
            .collect();
        assert_eq!(
            pick_latest_asset(&r, "dlssnr-").unwrap().0,
            "dlssnr-310.8.SF-v2"
        );
        assert!(pick_latest_asset(&r, "renodx-dlss5-").is_err());
    }

    #[test]
    fn latest_asset_versions_and_prefix_isolation() {
        let r = rhi_releases();
        assert_eq!(
            pick_latest_asset(&r, "renodx-dlss5-").unwrap().0,
            "renodx-dlss5-4.55"
        );
        assert_eq!(
            pick_latest_asset(&r, "dlssnr-").unwrap().0,
            "dlssnr-310.8.SF-v2"
        );
        assert_eq!(pick_latest_asset(&r, "dlss-").unwrap().0, "dlss-310.8.0");
        assert!(pick_latest_asset(&r, "nothing-").is_err());
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])], prefix: &[u8]) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(prefix).unwrap();
        let mut w = zip::ZipWriter::new(f);
        for (name, data) in entries {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn vulkan_feeder_kit_writes_addon_fx_and_note() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let z = t.path().join("feeder.zip");
        write_zip(
            &z,
            &[(game::FEEDER_ADDON, b"addon"), (game::FEEDER_FX, b"fx")],
            &[],
        );
        let out = copy_vulkan_feeder_kit_from_zip(&z, d, "v0.14.0").unwrap();
        assert!(d.join(game::FEEDER_ADDON).is_file());
        assert!(d
            .join("reshade-shaders")
            .join("Shaders")
            .join(game::FEEDER_FX)
            .is_file());
        assert!(d.join("VULKAN-SETUP.txt").is_file());
        assert!(out.iter().any(|s| s.contains("VULKAN-SETUP")));
        assert!(out.iter().any(|s| s.contains("v0.14.0")));
    }

    #[test]
    fn dgvoodoo_from_zip_writes_d3d9_and_conf_ignores_off() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        // Old ReShade rename must not be restored as dgVoodoo.
        fs::write(d.join("d3d9.dll.off"), b"MZ old reshade not dgVoodoo").unwrap();
        let z = d.join("dgVoodoo2_87_3.zip");
        write_zip(
            &z,
            &[(
                "MS/x86/D3D9.dll",
                b"MZ....dgVoodoo2 wrapper bytes for detect....",
            )],
            &[],
        );
        let out = install_dgvoodoo_from_zip(&z, d, 32).unwrap();
        assert!(out.contains(&"d3d9.dll".to_string()));
        assert!(out.contains(&"dgVoodoo.conf".to_string()));
        let dll = fs::read(d.join("d3d9.dll")).unwrap();
        assert!(dll.windows(8).any(|w| w.eq_ignore_ascii_case(b"dgVoodoo")));
        assert_ne!(
            fs::read(d.join("d3d9.dll.off")).unwrap(),
            dll,
            "must not restore d3d9.dll.off"
        );
        let conf = fs::read_to_string(d.join("dgVoodoo.conf")).unwrap();
        assert!(conf.contains("OutputAPI = d3d11_fl11_0"));
        assert!(conf.contains("VRAM = 4096"));
        assert!(conf.contains("Antialiasing = appdriven"));
        assert!(conf.contains("FastVideoMemoryAccess = false"));
        assert!(game::is_dgvoodoo(d));
    }

    #[test]
    fn dgvoodoo_conf_merge_preserves_user_keys_and_floors_vram() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        fs::write(
            d.join("dgVoodoo.conf"),
            "[General]\nOutputAPI = bestavailable\nAdapters = all\n\
             [DirectX]\nVRAM = 256\nFiltering = force16bit\n",
        )
        .unwrap();
        write_dgvoodoo_conf(d).unwrap();
        assert!(d.join("dgVoodoo.conf.bak").is_file());
        let conf = fs::read_to_string(d.join("dgVoodoo.conf")).unwrap();
        assert!(conf.contains("OutputAPI = d3d11_fl11_0"));
        assert!(!conf.to_ascii_lowercase().contains("bestavailable"));
        assert!(conf.contains("VRAM = 4096"));
        assert!(conf.contains("Filtering = force16bit"));
        assert!(conf.contains("Adapters = all"));
        // Second merge must not overwrite bak with already-merged text.
        let bak1 = fs::read(d.join("dgVoodoo.conf.bak")).unwrap();
        write_dgvoodoo_conf(d).unwrap();
        assert_eq!(fs::read(d.join("dgVoodoo.conf.bak")).unwrap(), bak1);
        // Keep a higher user VRAM.
        fs::write(
            d.join("dgVoodoo.conf"),
            "[General]\nOutputAPI = d3d11_fl11_0\n[DirectX]\nVRAM = 8192\n",
        )
        .unwrap();
        write_dgvoodoo_conf(d).unwrap();
        let conf2 = fs::read_to_string(d.join("dgVoodoo.conf")).unwrap();
        assert!(conf2.contains("VRAM = 8192"));
    }

    #[test]
    fn dgvoodoo_from_zip_picks_x64_member() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let z = d.join("dgVoodoo2_87_3.zip");
        write_zip(
            &z,
            &[(
                "MS/x64/D3D9.dll",
                b"MZ....dgVoodoo2 wrapper bytes for detect....",
            )],
            &[],
        );
        install_dgvoodoo_from_zip(&z, d, 64).unwrap();
        assert!(game::is_dgvoodoo(d));
    }

    #[test]
    fn plan_puts_dgvoodoo_first_for_dx9() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe_with_imports(&t.path().join("g3.exe"), game::PE_X86, &["engine.dll"]);
        make_pe_with_imports(
            &t.path().join("Engine.dll"),
            game::PE_X86,
            &["d3d9.dll", "kernel32.dll"],
        );
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = game::inspect(&exe).unwrap();
        assert!(st.needs_dgvoodoo());
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[0], "dgVoodoo 2.87.3 (DX9 → D3D11)");
        assert!(names.iter().any(|n| n.starts_with("ReShade")));
    }

    #[test]
    fn plan_refreshes_dgvoodoo_conf_when_already_present() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe_with_imports(&d.join("g3.exe"), game::PE_X86, &["engine.dll"]);
        make_pe_with_imports(
            &d.join("Engine.dll"),
            game::PE_X86,
            &["d3d9.dll", "kernel32.dll"],
        );
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        fs::write(
            d.join("dgVoodoo.conf"),
            b"[General]\nOutputAPI = bestavailable\n",
        )
        .unwrap();
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let st = game::inspect(&exe).unwrap();
        assert!(!st.needs_dgvoodoo());
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[0], "dgVoodoo 2.87.3 (DX9 → D3D11)");
    }

    #[test]
    fn reshade_from_setup_exe_with_prepended_stub() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let setup = t.path().join("ReShade_Setup_6.8.0_Addon.exe");
        let mut dll = b"MZ".to_vec();
        dll.extend(std::iter::repeat_n(0u8, 1 << 20));
        dll.extend_from_slice(b"ReShade");
        write_zip(
            &setup,
            &[("ReShade64.dll", &dll), ("ReShade32.dll", b"32")],
            &[b'M', b'Z', 0, 0, 0, 0, 0, 0],
        );
        assert_eq!(
            install_reshade_from_setup(&setup, t.path(), 64, game::RESHADE_PROXY).unwrap(),
            vec!["dxgi.dll"]
        );
        assert!(game::inspect(&exe).unwrap().reshade);
    }

    #[test]
    fn lumenite_zip_places_shaders_includes_texture_and_ignores_slip() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let z = t.path().join("LumeniteFX.zip");
        write_zip(
            &z,
            &[
                ("LumeniteFX-mainline/README.md", b"x"),
                (
                    "LumeniteFX-mainline/Shaders/lumenite_Kernel.fx",
                    b"technique Lumenite_Kernel {}",
                ),
                ("LumeniteFX-mainline/Shaders/lumenite_TRAA.fx", b"t"),
                (
                    "LumeniteFX-mainline/Shaders/include/lumenite_Helpers.fxh",
                    b"h",
                ),
                (
                    "LumeniteFX-mainline/Textures/lumenite_bluenoise256.png",
                    b"png",
                ),
                ("../evil.fx", b"zip-slip"),
            ],
            &[],
        );
        let installed = install_lumenite_from_zip(&z, t.path()).unwrap();
        assert!(
            installed.len() >= 4,
            "expected at least Kernel/TRAA/include/png, got {installed:?}"
        );
        assert!(t
            .path()
            .join("reshade-shaders/Shaders/lumenite_Kernel.fx")
            .is_file());
        assert!(t
            .path()
            .join("reshade-shaders/Shaders/include/lumenite_Helpers.fxh")
            .is_file());
        assert!(t
            .path()
            .join("reshade-shaders/Textures/lumenite_bluenoise256.png")
            .is_file());
        assert!(!t.path().parent().unwrap().join("evil.fx").exists());
        assert!(game::inspect(&exe).unwrap().lumenite);

        let bad = t.path().join("bad.zip");
        write_zip(&bad, &[("whatever.txt", b"x")], &[]);
        assert!(install_lumenite_from_zip(&bad, t.path()).is_err());
    }

    #[test]
    fn traa_ui_protect_patch_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let shaders = t.path().join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&shaders).unwrap();
        // Anchors must match stock lumenite_TRAA.fx (LumeniteFX mainline).
        let body = concat!(
            "uniform int EDGE_MODE <\n",
            "    ui_tooltip = \"Luma: shading and texture edges as well; the classic DLAA mask.\\n\"\n",
            "                 \"Geometric: silhouettes only, ignores flat UI.\";\n",
            "    > = 0;\n",
            "/*--------------.\n",
            "| :: IMPORTS :: |\n",
            "'--------------*/\n",
            "namespace Kernel {}\n",
            "namespace LumeniteTRAA {\n",
            "    confidence = saturate(confidence + 0.11 * 4.0 * confidence * (1.0 - confidence));\n",
            "\n",
            "    float2 historyUV = texcoord + flow;\n",
            "technique Lumenite_TRAA <\n",
            "    ui_tooltip = \"Temporal Reprojection Anti-Aliasing.\";\n",
            ">\n",
            "}\n",
        );
        let dest = shaders.join("lumenite_TRAA.fx");
        fs::write(&dest, body).unwrap();
        let first = apply_traa_ui_patch(t.path()).unwrap().unwrap();
        assert!(first.contains("UI protect patch"), "{first}");
        let text = fs::read_to_string(&dest).unwrap();
        assert!(text.contains("DLSS5_TRAA_UI_PROTECT"));
        assert!(text.contains("UI_PROTECT"));
        assert!(text.contains("> = 1;"));
        let second = apply_traa_ui_patch(t.path()).unwrap().unwrap();
        assert!(second.contains("already applied"), "{second}");
    }

    #[test]
    fn single_from_zip_and_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let z = t.path().join("renodx-dlss5-4.55.zip");
        write_zip(&z, &[("renodx-dlss5.addon64", b"addon")], &[]);
        install_single_from_zip(
            &z,
            "renodx-dlss5.addon64",
            &t.path().join("renodx-dlss5.addon64"),
        )
        .unwrap();
        assert!(game::inspect(&exe).unwrap().dlss5_addon);
        fs::write(t.path().join(game::DLSS_DLL), b"keep").unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&"renodx-dlss5.addon64".to_string()));
        assert!(t.path().join(game::DLSS_DLL).is_file());
    }

    #[test]
    fn thirty_two_bit_plan_status_and_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game32.exe"), game::PE_X86);
        let st = game::inspect(&exe).unwrap();
        assert!(st.is32());
        assert_eq!(st.mode, game::Mode::Feeder);
        assert!(st.problems.is_empty(), "{:?}", st.problems);
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[0], "ReShade (add-on build)");
        assert_eq!(names[1], "64-bit ReShade for the host64 helper");
        assert_eq!(names.len(), 8); // + the GPU-preference step
                                    // Lay the 32-bit result out by hand and check status + removal.
        let d = t.path();
        let host = d.join(game::HOST_DIR);
        fs::create_dir_all(d.join("reshade-shaders").join("Shaders")).unwrap();
        fs::create_dir_all(&host).unwrap();
        fs::write(d.join(game::FEEDER_ADDON32), b"a32").unwrap();
        fs::write(
            d.join("reshade-shaders")
                .join("Shaders")
                .join(game::FEEDER_FX),
            b"fx",
        )
        .unwrap();
        fs::write(host.join(game::HOST_EXE), b"host").unwrap();
        fs::write(host.join(game::DLSS5_ADDON), b"addon").unwrap();
        fs::write(host.join(game::DLSSNR_DLL), b"nr").unwrap();
        fs::write(host.join(game::DLSS_DLL), b"dlss").unwrap();
        fs::write(host.join(game::DLSS_MARKER), b"dlss-1").unwrap();
        make_reshade_dll(&host.join(game::RESHADE_PROXY));
        fs::write(host.join(game::RESHADE_MARKER), b"6.8.0").unwrap();
        let st = game::inspect(&exe).unwrap();
        assert!(
            st.feeder && st.dlss5_addon && st.dlssnr && st.dlss && st.host_exe && st.host_reshade
        );
        let removed = uninstall(&exe).unwrap();
        assert!(removed.iter().any(|r| r.contains(game::HOST_EXE)));
        assert!(!host.exists(), "host64 folder should be gone: {removed:?}");
        assert!(!d.join(game::FEEDER_ADDON32).exists());
    }

    #[test]
    fn plan_adds_reframework_first_and_renodx_before_config() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("re4.exe"), game::PE_X64);
        fs::write(t.path().join(game::RE_ENGINE_PAK), b"pak").unwrap();
        let mut st = game::inspect(&exe).unwrap();
        assert!(st.re_engine && !st.reframework);
        st.mode = game::Mode::Native;
        st.api = game::Api::Dx12;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras { with_renodx: true, ..Default::default() })
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "REFramework (RE Engine needs it before ReShade)",
                "ReShade (add-on build)",
                "DLSS 5 add-on + models",
                "RenoDX HDR mod for this game",
                "ReShade config",
                "GPU preference"
            ]
        );
        let names: Vec<&str> = plan_with(&st, Engine::Opti, Extras { with_renodx: true, ..Default::default() })
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "REFramework (RE Engine needs it before ReShade)",
                "OptiScaler + DLSS Neural Rendering",
                "DLSS 5 model (nvngx_dlssnr.dll)",
                "ReShade loaded by OptiScaler (ReShade64.dll)",
                "RenoDX HDR mod for this game",
                "GPU preference"
            ]
        );
    }

    #[test]
    fn set_ini_key_is_section_scoped_and_appends() {
        // The RE Engine hotfixes go under [Hotfix]; the same key names exist
        // elsewhere in OptiScaler.ini, so only that section may move (#44).
        let ini = "[Menu]
ManualInputPolling=auto

[Hotfix]
ManualInputPolling=auto
ExtendedStateRestore=true
";
        let out = set_ini_key(ini, "Hotfix", "ManualInputPolling", "true").unwrap();
        assert_eq!(
            out,
            "[Menu]
ManualInputPolling=auto

[Hotfix]
ManualInputPolling=true
ExtendedStateRestore=true
"
        );
        // A value that already reads that way is left alone.
        assert!(set_ini_key(&out, "Hotfix", "ManualInputPolling", "true").is_none());
        // Turning one back off is the same operation.
        let off = set_ini_key(&out, "Hotfix", "ExtendedStateRestore", "false").unwrap();
        assert!(off.contains("ExtendedStateRestore=false"));
        // Missing section is appended rather than dropped.
        let added = set_ini_key(
            "[Menu]
X=1
",
            "Hotfix",
            "RestoreComputeSignature",
            "true",
        )
        .unwrap();
        assert!(added.ends_with(
            "
[Hotfix]
RestoreComputeSignature=true
"
        ));
    }

    /// The model-resolution dial is the biggest performance lever on the
    #[test]
    fn dlss_nr_enabled_is_section_scoped() {
        // "Enabled" also lives under other headings; only DlssNr's may move.
        let ini = "[OptiFG]\nEnabled=auto\n\n[DlssNr]\n; comment\nEnabled=auto\n";
        assert_eq!(
            set_dlss_nr_enabled(ini).unwrap(),
            "[OptiFG]\nEnabled=auto\n\n[DlssNr]\n; comment\nEnabled=true\n"
        );
        assert!(set_dlss_nr_enabled("[DlssNr]\nEnabled=true\n").is_none());
        // No section at all: append one.
        assert_eq!(
            set_dlss_nr_enabled("[OptiFG]\nEnabled=auto\n").unwrap(),
            "[OptiFG]\nEnabled=auto\n\n[DlssNr]\nEnabled=true\n"
        );
    }

    #[test]
    fn set_load_reshade_rewrites_or_appends() {
        let ini = "[Plugins]\r\n; doc\r\nLoadReshade=auto\r\nOther=1\r\n";
        assert_eq!(
            set_load_reshade(ini).unwrap(),
            "[Plugins]\r\n; doc\r\nLoadReshade=true\r\nOther=1\r\n"
        );
        assert!(set_load_reshade("LoadReshade=true\n").is_none());
        assert_eq!(
            set_load_reshade("[Upscalers]\nDx12Upscaler=auto\n").unwrap(),
            "[Upscalers]\nDx12Upscaler=auto\n\n[Plugins]\nLoadReshade=true\n"
        );
    }

    #[test]
    fn uninstall_removes_recorded_renodx_mod_and_reframework_only() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        fs::write(t.path().join("renodx-cp2077.addon64"), b"ours").unwrap();
        fs::write(t.path().join("renodx-ff7rebirth.addon64"), b"theirs").unwrap();
        fs::write(
            t.path().join(game::RENODX_MANIFEST),
            "renodx-cp2077.addon64\n",
        )
        .unwrap();
        fs::write(t.path().join(game::REFRAMEWORK_DLL), b"ref").unwrap();
        let st = game::inspect(&exe).unwrap();
        assert_eq!(st.renodx_mod.as_deref(), Some("renodx-cp2077.addon64"));
        assert_eq!(
            st.foreign_renodx,
            vec!["renodx-ff7rebirth.addon64".to_string()]
        );
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&"renodx-cp2077.addon64".to_string()));
        assert!(t.path().join("renodx-ff7rebirth.addon64").is_file());
        // dinput8.dll without our marker is somebody else's REFramework: kept.
        assert!(t.path().join(game::REFRAMEWORK_DLL).is_file());
        fs::write(t.path().join(game::REFRAMEWORK_MARKER), b"").unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.contains(&game::REFRAMEWORK_DLL.to_string()));
    }

    /// Upstream publishes some "-beta" tags with prerelease=false, so the tag
    /// name is what decides whether the install log says beta.
    /// Only a component this tool recorded can be reported as out of date;
    /// a user's own ReShade has no marker and must stay invisible.
    #[test]
    fn stale_components_reports_only_what_we_placed() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let latest = Latest {
            reshade: Some("6.8.0".into()),
            feeder: Some("v0.13.1-beta.1".into()),
            opti: Some("v0.2.0-dlssnr".into()),
            opti_presr: Some("v1.1.0".into()),
            dlss: Some("dlss-310.9.0".into()),
            dlssnr: Some("dlssnr-310.8.SF-v2".into()),
        };
        assert!(stale_components(d, &latest).is_empty());

        fs::write(d.join(game::FEEDER_MARKER), "v0.12.0").unwrap();
        fs::write(d.join(game::DLSS_MARKER), "dlss-310.9.0").unwrap();
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.1.2-dlssnr\ndxgi.dll\n",
        )
        .unwrap();
        let stale = stale_components(d, &latest);
        assert_eq!(
            stale,
            vec![
                "DLSS5-Feeder v0.12.0 → v0.13.1-beta.1".to_string(),
                "OptiScaler v0.1.2-dlssnr → v0.2.0-dlssnr".to_string(),
            ]
        );

        // A manifest from before the tag was recorded cannot be compared, and
        // saying nothing would leave a stale install looking current.
        fs::write(d.join(game::OPTI_MANIFEST), "dxgi.dll\nOptiScaler.ini\n").unwrap();
        assert!(stale_components(d, &latest)
            .iter()
            .any(|s| s == "OptiScaler unknown version → v0.2.0-dlssnr"));
        assert!(!installed_opti_presr(d));

        // A pre-SR multipass install is measured against its own fork's newest
        // build, never Dagherbou's differently numbered one.
        fs::write(
            d.join(game::OPTI_MANIFEST),
            format!("# repo {OPTI_PRESR_REPO}\n# tag v1.1.0\ndxgi.dll\n"),
        )
        .unwrap();
        assert!(!stale_components(d, &latest)
            .iter()
            .any(|s| s.starts_with("OptiScaler")));
        fs::write(
            d.join(game::OPTI_MANIFEST),
            format!("# repo {OPTI_PRESR_REPO}\n# tag v1.0.0\ndxgi.dll\n"),
        )
        .unwrap();
        assert!(stale_components(d, &latest)
            .iter()
            .any(|s| s == "OptiScaler v1.0.0 → v1.1.0"));
        assert!(installed_opti_presr(d));
    }

    /// The two builds number their releases independently, so the pre-SR fork's
    /// v0.7.7 was compared against the stable build's v0.2.0-dlssnr and reported
    /// an update on every single run, which Install could never clear (#88).
    #[test]
    fn the_presr_fork_is_compared_against_its_own_releases() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let latest = Latest {
            reshade: None,
            feeder: None,
            opti: Some("v0.2.0-dlssnr".into()),
            opti_presr: Some("v0.7.7".into()),
            dlss: None,
            dlssnr: None,
        };

        // Current pre-SR install: the repo line settles it.
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.7.7\n# repo wilsjo2/OptiScaler-DLSSNR-PreSR-Multipass\ndxgi.dll\n",
        )
        .unwrap();
        assert!(stale_components(d, &latest).is_empty());

        // Behind on the pre-SR fork: named against that fork's newest.
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.7.6\n# repo wilsjo2/OptiScaler-DLSSNR-PreSR-Multipass\ndxgi.dll\n",
        )
        .unwrap();
        assert_eq!(
            stale_components(d, &latest),
            vec!["OptiScaler v0.7.6 → v0.7.7".to_string()]
        );

        // A manifest from before the repo line: the stable build's tags all end
        // in "-dlssnr", so the shape of the tag says which build it is.
        fs::write(d.join(game::OPTI_MANIFEST), "# tag v0.7.7\ndxgi.dll\n").unwrap();
        assert!(stale_components(d, &latest).is_empty());
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "# tag v0.2.0-dlssnr\ndxgi.dll\n",
        )
        .unwrap();
        assert!(stale_components(d, &latest).is_empty());
    }

    /// The manifest carries the tag on a comment line, and older manifests
    /// (written before that) must read as "unknown" rather than as a path.
    #[test]
    fn manifest_tag_is_read_from_the_header() {
        let m = "# tag v0.2.0-dlssnr\nOptiScaler.dll\ndxgi.dll\n";
        assert_eq!(manifest_tag(m).as_deref(), Some("v0.2.0-dlssnr"));
        assert_eq!(manifest_tag("OptiScaler.dll\ndxgi.dll\n"), None);
        // The fork rides alongside; a manifest from before it was recorded
        // reads as Dagherbou's build, the default.
        assert_eq!(manifest_repo(m), OPTI_REPO);
        let presr = format!("# repo {OPTI_PRESR_REPO}\n{m}");
        assert_eq!(manifest_repo(&presr), OPTI_PRESR_REPO);
        assert_eq!(manifest_tag(&presr).as_deref(), Some("v0.2.0-dlssnr"));
    }

    #[test]
    fn prerelease_tags_are_named_by_their_tag() {
        assert!(is_prerelease_tag("v0.13.1-beta.1"));
        assert!(is_prerelease_tag("v0.12.1-beta.2"));
        assert!(is_prerelease_tag("v1.0.0-rc.1"));
        assert!(!is_prerelease_tag("v0.12.0"));
        assert!(!is_prerelease_tag("v1.4.8"));
    }

    #[test]
    fn plan_follows_mode_and_api() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let mut st = game::inspect(&exe).unwrap();
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names.len(), 7); // + the GPU-preference step
        assert_eq!(names[2], "DLSS5-Feeder");
        st.mode = game::Mode::Native;
        st.api = game::Api::Dx12;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "ReShade (add-on build)",
                "DLSS 5 add-on + models",
                "ReShade config",
                "GPU preference"
            ]
        );
        st.api = game::Api::Dx11;
        let names: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names[2], "DLSS 5 DX11 bridge");
    }

    #[test]
    fn uninstall_all_removes_reshade_even_with_leftover_shaders() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        fs::write(d.join(game::RESHADE_MARKER), b"6.8.0").unwrap();
        let sh = d.join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&sh).unwrap();
        fs::write(d.join(game::FEEDER_ADDON), b"x").unwrap();
        fs::write(sh.join(game::FEEDER_FX), b"x").unwrap();
        fs::write(sh.join("ReShade.fxh"), b"x").unwrap();
        fs::write(d.join("ReShade.ini"), b"x").unwrap();
        fs::write(d.join("ReShadePreset.ini"), b"x").unwrap();
        fs::write(d.join("dlss5-feed.cfg"), b"x").unwrap();
        // Pre-existing shader pack (Gothic 3 etc.) must not block dxgi.dll removal.
        fs::write(sh.join("Clarity.fx"), b"user shader").unwrap();
        // dgVoodoo for DX9 games must never be touched.
        fs::write(d.join("d3d9.dll"), b"MZ...dgVoodoo2 wrapper...").unwrap();
        fs::write(
            d.join("dgVoodoo.conf"),
            b"[DirectX]\nOutputAPI = bestavailable\n",
        )
        .unwrap();

        let (removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_none(), "{kept:?}");
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join(game::RESHADE_MARKER).exists());
        assert!(!d.join("ReShade.ini").exists());
        assert!(!d.join("dlss5-feed.cfg").exists());
        assert!(!d.join(game::FEEDER_ADDON).is_file());
        assert!(d
            .join("reshade-shaders")
            .join("Shaders")
            .join("Clarity.fx")
            .is_file());
        assert!(d.join("d3d9.dll").is_file(), "dgVoodoo d3d9.dll must stay");
        assert!(d.join("dgVoodoo.conf").is_file());
        assert!(!game::inspect(&exe).unwrap().reshade);
    }

    #[test]
    fn uninstall_all_cleans_empty_reshade_shaders_tree() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        let sh = d.join("reshade-shaders").join("Shaders");
        fs::create_dir_all(&sh).unwrap();
        fs::write(sh.join("ReShade.fxh"), b"x").unwrap();
        fs::write(d.join("ReShade.ini"), b"x").unwrap();
        let (removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_none(), "{kept:?}");
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join("ReShade.ini").exists());
        assert!(!d.join("reshade-shaders").exists());
    }

    #[test]
    fn uninstall_all_keeps_foreign_addons() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        crate::game::testutil::make_reshade_dll(&d.join("dxgi.dll"));
        fs::write(d.join("someones-mod.addon64"), b"x").unwrap();
        let (_removed, kept) = uninstall_all(&exe).unwrap();
        assert!(kept.is_some());
        assert!(d.join("dxgi.dll").is_file());
    }

    /// Neural Upstream is the neural consumer itself, so it takes the RenoDX
    /// add-on's place in the plan rather than being added next to it, and it
    /// needs the model beside it (#50).
    #[test]
    fn upstream_plan_replaces_the_renodx_consumer() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let mut st = game::inspect(&exe).unwrap();
        st.mode = game::Mode::Native;
        let stable: Vec<&str> = plan_with(&st, Engine::ReShade, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        let upstream: Vec<&str> = plan_with(&st, Engine::ReShade, Extras { upstream: true, ..Default::default() })
            .iter()
            .map(|s| s.name)
            .collect();
        assert!(stable.contains(&"DLSS 5 add-on + models"));
        assert!(!stable.contains(&"Neural Upstream add-on (experimental)"));
        assert!(upstream.contains(&"Neural Upstream add-on (experimental)"));
        assert!(upstream.contains(&"DLSS 5 model (nvngx_dlssnr.dll)"));
        assert!(!upstream.contains(&"DLSS 5 add-on + models"));
    }

    /// It reads the colour buffer the game hands its own DLSS, so a game
    /// without DLSS cannot feed it: refuse by name instead of installing.
    #[test]
    fn upstream_refuses_a_game_with_no_dlss_of_its_own() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let e = run_all_with(
            &exe,
            Engine::ReShade,
            Extras {
                upstream: true,
                ..Default::default()
            },
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("needs a game with DLSS of its own"),
            "{e:#}"
        );
    }

    #[test]
    fn opti_plan_and_engine_gate() {
        std::env::set_var("DLSS5ONECLICK_SKIP_GPU_CHECK", "1");
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X64);
        let st = game::inspect(&exe).unwrap();
        let names: Vec<&str> = plan_with(&st, Engine::Opti, Extras::default())
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            [
                "OptiScaler + DLSS Neural Rendering",
                "DLSS 5 model (nvngx_dlssnr.dll)",
                "GPU preference"
            ]
        );
        // Feeder-mode game + Opti engine is refused before any network
        let err = run_all_with(
            &exe,
            Engine::Opti,
            Extras::default(),
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("own DLSS"));
    }

    #[test]
    fn uninstall_removes_opti_manifest_files() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let exe = make_pe(&d.join("game.exe"), game::PE_X64);
        fs::create_dir_all(d.join("OptiScaler")).unwrap();
        fs::write(d.join("dxgi.dll"), b"opti").unwrap();
        fs::write(d.join("OptiScaler.ini"), b"ini").unwrap();
        fs::write(d.join("OptiScaler").join("libxess.dll"), b"x").unwrap();
        fs::write(
            d.join(game::OPTI_MANIFEST),
            "dxgi.dll\nOptiScaler.ini\nOptiScaler/libxess.dll",
        )
        .unwrap();
        let removed = uninstall(&exe).unwrap();
        assert!(removed.iter().any(|r| r == "dxgi.dll"));
        assert!(!d.join("dxgi.dll").exists());
        assert!(!d.join("OptiScaler").exists());
        assert!(!d.join(game::OPTI_MANIFEST).exists());
    }

    #[test]
    fn run_all_refuses_opti_on_32bit_before_network() {
        let t = tempfile::tempdir().unwrap();
        let exe = make_pe(&t.path().join("game.exe"), game::PE_X86);
        let err = run_all_with(
            &exe,
            Engine::Opti,
            Extras::default(),
            InstallOpts {
                quality: QualityChoice::Auto,
                overrides: QualityOverrides::default(),
            },
            &|_, _| {},
            &|_, _, _, _, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("64-bit only"));
    }
}
