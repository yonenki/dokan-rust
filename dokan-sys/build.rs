use std::{
	collections::BTreeSet,
	env,
	error::Error,
	ffi::OsStr,
	fs,
	path::{Path, PathBuf},
	process::{Command, Stdio},
};

use cc::{Build, Tool};
use serde::Deserialize;

const DEFAULT_DISTRIBUTION_PROFILE: &str = "src/dokany/profiles/upstream.json";
const DOKANY_SOURCE_COMMIT_PATH: &str = "src/dokany-source-commit.txt";
const DISTRIBUTION_PROFILE_ENV: &str = "DOKAN_DISTRIBUTION_PROFILE";
const DLL_OUTPUT_PATH_ENV: &str = "DOKAN_DLL_OUTPUT_PATH";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeIdentityManifest {
	product_version: String,
	family: RuntimeFamilyManifest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeFamilyManifest {
	binary_base_name: String,
}

fn run_checked(command: &mut Command, description: &str) -> Result<(), Box<dyn Error>> {
	let status = command
		.stdout(Stdio::inherit())
		.stderr(Stdio::inherit())
		.status()?;
	if !status.success() {
		return Err(format!("{description} failed with {status}").into());
	}
	Ok(())
}

fn generate_distribution_profile(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
	let profile = env::var_os(DISTRIBUTION_PROFILE_ENV)
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from(DEFAULT_DISTRIBUTION_PROFILE));
	let generated_dir = out_dir.join("distribution-profile");
	let source_commit = fs::read_to_string(DOKANY_SOURCE_COMMIT_PATH)?;
	let source_commit = source_commit.trim();
	if source_commit.len() != 40
		|| !source_commit
			.bytes()
			.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
	{
		return Err("pinned Dokany source commit is not a full Git object ID".into());
	}

	println!("cargo:rerun-if-env-changed={DISTRIBUTION_PROFILE_ENV}");
	println!("cargo:rerun-if-changed={DOKANY_SOURCE_COMMIT_PATH}");
	println!("cargo:rerun-if-changed={}", profile.display());
	println!("cargo:rerun-if-changed=src/dokany/tools/DistributionProfile");
	println!("cargo:rerun-if-changed=src/dokany/profiles");

	run_checked(
		Command::new("dotnet")
			.arg("run")
			.arg("--project")
			.arg("src/dokany/tools/DistributionProfile/DistributionProfile.csproj")
			.arg("--")
			.arg("generate")
			.arg(&profile)
			.arg(&generated_dir)
			.arg(source_commit),
		"Dokany distribution profile generation",
	)?;

	Ok(generated_dir)
}

fn read_runtime_manifest(generated_dir: &Path) -> Result<RuntimeIdentityManifest, Box<dyn Error>> {
	let manifest = fs::read(generated_dir.join("runtime-identity.json"))?;
	Ok(serde_json::from_slice(&manifest)?)
}

fn generate_rust_version_constants(
	compiler: &Tool,
	out_dir: &Path,
	generated_dir: &Path,
) -> Result<String, Box<dyn Error>> {
	let executable = out_dir.join("generate_version.exe");
	let mut command = compiler.to_command();
	command
		.arg("-Isrc/dokany/dokan")
		.arg("-Isrc/dokany/sys")
		.arg(format!("-I{}", generated_dir.display()));
	if compiler.is_like_msvc() {
		command
			.arg(format!("/Fo{}/", out_dir.display()))
			.arg("src/generate_version.c")
			.arg("/link")
			.arg(format!("/OUT:{}", executable.display()));
	} else {
		command
			.arg(format!("-o{}", executable.display()))
			.arg("src/generate_version.c");
	}
	run_checked(&mut command, "Dokany Rust constant generator compilation")?;
	run_checked(
		Command::new(&executable).current_dir(out_dir),
		"Dokany Rust constant generation",
	)?;
	println!("cargo:rerun-if-changed=src/generate_version.c");

	Ok(fs::read_to_string(out_dir.join("version.txt"))?)
}

fn c_sources() -> Result<Vec<PathBuf>, Box<dyn Error>> {
	let mut sources = fs::read_dir("src/dokany/dokan")?
		.map(|entry| entry.map(|entry| entry.path()))
		.collect::<Result<Vec<_>, _>>()?;
	sources.retain(|path| path.extension() == Some(OsStr::new("c")));
	sources.sort();
	Ok(sources)
}

fn compile_version_resource(
	compiler: &Tool,
	out_dir: &Path,
	generated_dir: &Path,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
	if !compiler.is_like_msvc() {
		println!("cargo:warning=Dokany DLL version resources are only compiled for MSVC targets");
		return Ok(None);
	}

	let program_files_x86 = env::var_os("ProgramFiles(x86)")
		.ok_or("ProgramFiles(x86) is not set; cannot locate the Windows SDK")?;
	let sdk_bin_root = PathBuf::from(program_files_x86).join("Windows Kits/10/bin");
	let host_tools_arch = if env::var("HOST")?.starts_with("x86_64-") {
		"x64"
	} else {
		"x86"
	};
	let mut sdk_versions = fs::read_dir(&sdk_bin_root)?
		.map(|entry| entry.map(|entry| entry.path()))
		.collect::<Result<Vec<_>, _>>()?;
	sdk_versions.retain(|path| path.join(host_tools_arch).join("rc.exe").is_file());
	sdk_versions.sort();
	let resource_compiler = sdk_versions
		.last()
		.map(|path| path.join(host_tools_arch).join("rc.exe"))
		.ok_or("Windows SDK resource compiler rc.exe was not found")?;
	let resource = out_dir.join("dokan.res");
	let mut command = Command::new(resource_compiler);
	for (name, value) in compiler.env() {
		command.env(name, value);
	}
	command
		.arg("/nologo")
		.arg(format!("/I{}", generated_dir.display()))
		.arg("/Isrc/dokany/dokan")
		.arg(format!("/fo{}", resource.display()))
		.arg("src/dokany/dokan/dokan.rc");
	run_checked(&mut command, "Dokany version resource compilation")?;
	println!("cargo:rerun-if-changed=src/dokany/dokan/dokan.rc");
	Ok(Some(resource))
}

fn build_dokan(
	compiler: &Tool,
	out_dir: &Path,
	generated_dir: &Path,
	binary_base_name: &str,
) -> Result<(), Box<dyn Error>> {
	let dll_name = format!("{binary_base_name}.dll");
	let dll_path = out_dir.join(&dll_name);
	let import_library = out_dir.join(format!("{binary_base_name}.lib"));
	let sources = c_sources()?;
	let version_resource = compile_version_resource(compiler, out_dir, generated_dir)?;
	let mut command = compiler.to_command();
	command
		.arg("-D_WINDLL")
		.arg("-D_EXPORTING")
		.arg("-DUNICODE")
		.arg("-D_UNICODE")
		.arg("-DWINVER=0x0A00")
		.arg("-D_WIN32_WINNT=0x0A00")
		.arg("-Isrc/dokany/sys")
		.arg(format!("-I{}", generated_dir.display()));
	if compiler.is_like_msvc() {
		command
			.arg(format!("/Fo{}/", out_dir.display()))
			.args(&sources)
			.arg("/link")
			.arg("/DLL")
			.arg("/DEF:src/dokany/dokan/dokan.def")
			.arg(format!("/OUT:{}", dll_path.display()))
			.arg(format!("/IMPLIB:{}", import_library.display()))
			.arg("advapi32.lib")
			.arg("shell32.lib")
			.arg("user32.lib");
		if let Some(version_resource) = &version_resource {
			command.arg(version_resource);
		}
	} else {
		command
			.arg("-shared")
			.arg(format!("-o{}", dll_path.display()))
			.args(&sources)
			.arg(format!("-Wl,--out-implib,{}", import_library.display()));
	}
	run_checked(&mut command, "Dokany user-mode library build")?;

	println!("cargo:rerun-if-env-changed={DLL_OUTPUT_PATH_ENV}");
	println!("cargo:rerun-if-env-changed=CARGO_BUILD_BUILD_DIR");
	println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");
	for destination in runtime_dll_destinations(out_dir, &dll_name)? {
		fs::create_dir_all(
			destination
				.parent()
				.ok_or("Dokany runtime destination has no parent")?,
		)?;
		fs::copy(&dll_path, &destination)?;
	}

	println!("cargo:rustc-link-search=native={}", out_dir.display());
	println!("cargo:rerun-if-changed=src/dokany/dokan");
	println!("cargo:rerun-if-changed=src/dokany/sys");
	Ok(())
}

fn runtime_dll_destinations(
	out_dir: &Path,
	dll_name: &str,
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
	let profile_dir = out_dir
		.ancestors()
		.nth(3)
		.ok_or("OUT_DIR is not nested below a Cargo profile directory")?;
	let mut destinations = BTreeSet::from([
		profile_dir.join(dll_name),
		profile_dir.join("deps").join(dll_name),
	]);

	if let (Some(build_dir), Some(target_dir)) = (
		env::var_os("CARGO_BUILD_BUILD_DIR").map(PathBuf::from),
		env::var_os("CARGO_TARGET_DIR").map(PathBuf::from),
	) {
		if build_dir.is_absolute() && target_dir.is_absolute() {
			if let Ok(profile_suffix) = profile_dir.strip_prefix(build_dir) {
				if !profile_suffix.as_os_str().is_empty() {
					let target_profile_dir = target_dir.join(profile_suffix);
					destinations.insert(target_profile_dir.join(dll_name));
					destinations.insert(target_profile_dir.join("deps").join(dll_name));
				}
			}
		}
	}

	if let Some(output_path) = env::var_os(DLL_OUTPUT_PATH_ENV) {
		destinations.insert(PathBuf::from(output_path).join(dll_name));
	}
	Ok(destinations.into_iter().collect())
}

fn crate_dokany_version() -> Result<String, Box<dyn Error>> {
	let package_version = env::var("CARGO_PKG_VERSION")?;
	package_version
		.split_once("+dokan")
		.map(|(_, version)| version.to_owned())
		.ok_or_else(|| "crate version must end with +dokan<major><minor><patch>".into())
}

fn profile_dokany_version(product_version: &str) -> Result<String, Box<dyn Error>> {
	let components = product_version.split('.').take(3).collect::<Vec<_>>();
	if components.len() != 3 || components.iter().any(|component| component.len() != 1) {
		return Err(format!(
			"profile product version {product_version} cannot be encoded in the crate Dokany suffix"
		)
		.into());
	}
	Ok(components.concat())
}

fn main() -> Result<(), Box<dyn Error>> {
	let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
	let generated_dir = generate_distribution_profile(&out_dir)?;
	let manifest = read_runtime_manifest(&generated_dir)?;
	let compiler = Build::new().get_compiler();
	let generated_version = generate_rust_version_constants(&compiler, &out_dir, &generated_dir)?;

	let crate_version = crate_dokany_version()?;
	let profile_version = profile_dokany_version(&manifest.product_version)?;
	if generated_version != profile_version || generated_version != crate_version {
		return Err(format!(
			"Dokany version mismatch: source={generated_version}, profile={profile_version}, crate={crate_version}"
		)
		.into());
	}

	build_dokan(
		&compiler,
		&out_dir,
		&generated_dir,
		&manifest.family.binary_base_name,
	)?;
	println!(
		"cargo:rustc-link-lib=dylib={}",
		manifest.family.binary_base_name
	);
	Ok(())
}
