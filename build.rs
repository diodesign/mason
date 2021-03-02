/* Mason build script
 * 
 * This assembles low-level assembly code and package up binaries for linking with higher-level code.
 * It is a cargo-compatible super-build script. It reads the target architecture from the TARGET environment
 * variable set by cargo --target, and uses that to determine the tools it needs.
 * It assumes the necessary GNU binutils are present on the host system to assemble code and package binaries.
 * 
 * Mason is controlled by a TOML-compliant manifest configuration file named mason.toml.
 * It will search up the host file system tree from the current working directory for this file .
 * If no configuration file is found, Mason will exit with an error. The file format is:
 * 
 * defaults.include_files = array of binary file pathnames to link with the high-level code.
 * defaults.asm_dirs = array of directory pathnames of assembly source code to build and link with the high-level code.
 * target.<target architecture>.include_files = as for defaults but specific to the given architecture
 * target.<target architecture>.asm_dirs = as for defaults but specific to the given architecture
 *
 * All pathnames are relative to the current working directory. All the entries in the config file are optional.
 * The arrays also stack, meaning that if you define, eg, default and per-target asm_dirs entries, they will be
 * combined into one array and processed together. Mason ensures a path is included only once: multiple entries
 * of the same file path will be treated as one.
 * 
 * <target architecture> is specified by TARGET, eg: riscv64gc-unknown-none-elf 
 * Mason also uses the OUT_DIR environment variable, set by cargo, to write its files for linking.
 * 
 * Reminder: this runs on the host build system using the host's architecture.
 * Thus, a Rust toolchain that can build executables for the host arch must be installed, and
 * the host architecture must be the default toolchain target - or this script will fail.
 * For example: building a RISC-V kernel on an X86-64 server requires a toolchain that
 * can output code for both architectures, and outputs X86-64 by default.
 *
 * (c) Chris Williams, 2020.
 *
 * See README and LICENSE for usage and copying.
 */

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::collections::HashSet;
use std::collections::BTreeMap;

extern crate toml;
extern crate serde;
extern crate serde_derive;
use serde_derive::Deserialize;

extern crate regex;
use regex::Regex;

/* configuration file name */
static CONFIG_FILE: &str = "mason.toml";

/* max attempts to search the host file system for a config file */
static SEARCH_MAX: usize = 100;

/* define the structure of the configuration file */
#[derive(Deserialize)]
struct Config
{
    defaults: Option<ConfigEntry>,
    target: Option<BTreeMap<String, ConfigEntry>>
}

#[derive(Deserialize, Debug)]
struct ConfigEntry
{
    include_files: Option<Vec<String>>,
    asm_dirs: Option<Vec<String>>
}

/* describe a build target from its user-supplied triple */
struct Target
{
    pub cpu_arch: String,    /* define the CPU architecture to generate code for */
    pub gnu_prefix: String,  /* locate the GNU as and ar tools */ 
    pub platform: String,    /* locate the tail of the platform directory in src, eg riscv for src/platform-riscv */
    pub ptr_width: usize,    /* pointer width in bits */
    pub fp_width: usize,     /* floating-point register width in bits (or 0 for no FPU) */
    pub abi: String          /* define the ABI for this target */
}

impl Target
{
    /* create a target object from a full build triple string, taking the CPU arch from the first part of the triple  */
    pub fn new(triple: &String) -> Target
    {
        match triple.split('-').next().expect("Badly formatted target triple").as_ref()
        {
            "riscv64imac" => Target
            {
                cpu_arch: String::from("rv64imac"),
                gnu_prefix: String::from("riscv64"),
                platform: String::from("riscv"),
                ptr_width: 64,
                fp_width: 0,
                abi: String::from("lp64")
            },
            "riscv64gc" => Target
            {
                cpu_arch: String::from("rv64gc"),
                gnu_prefix: String::from("riscv64"),
                platform: String::from("riscv"),
                ptr_width: 64,
                fp_width: 64,
                abi: String::from("lp64")
            },
            unknown_target => panic!("Unsupported target '{}'", &unknown_target)
        }
    }
}

/* shared context of this build run */
pub struct Context<'a>
{
    /* defined by the host environment */
    output_dir: String,       /* where we're outputting object code on the host */
    objects: HashSet<String>, /* set of objects to link, referenced by their full path */
    as_exec: String,          /* path to target's GNU assembler executable */
    ar_exec: String,          /* path to target's GNU archiver executable */
    ld_exec: String,          /* path to target's GNU linker executable */
    oc_exec: String,          /* path to the target's GNU objcopy executable */
    target: &'a Target        /* describe the build target */
}

fn main()
{
    /* determine which CPU and platform we're building for from target triple */
    let target_string = env::var("TARGET").expect("Missing target triple, use --target with cargo");
    let target = Target::new(&target_string);

    /* create a shared context describing this build */
    let mut context = Context
    {
        output_dir: env::var("OUT_DIR").expect("No output directory specified"),
        objects: HashSet::new(),
        as_exec: String::from(format!("{}-linux-gnu-as", target.gnu_prefix)),
        ar_exec: String::from(format!("{}-linux-gnu-ar", target.gnu_prefix)),
        ld_exec: String::from(format!("{}-linux-gnu-ld", target.gnu_prefix)),
        oc_exec: String::from(format!("{}-linux-gnu-objcopy", target.gnu_prefix)),
        target: &target
    };

    /* get parsed contents of the config file, or bail out if this cannot be obtained */
    let config = parse_config_file();

    /* populate tables with paths of files to include and assemble from the config file */
    let mut include_files = HashSet::new();
    let mut asm_dirs = HashSet::new();

    /* include the defaults */
    if let Some(defaults) = config.defaults
    {
        add_file_paths_from_config(&defaults, &mut include_files, &mut asm_dirs);
    }

    /* select architecture's settings from the given target */
    if let Some(targets) = config.target
    {
        match targets.get(&target_string)
        {
            Some(arch) => add_file_paths_from_config(&arch, &mut include_files, &mut asm_dirs),
            None => ()
        }
    }

    /* package up individual binary files */
    for f in include_files
    {
        package_binary(&String::from(f), &mut context);
    }

    /* assemble all asm code in each of these directories */
    for dir in asm_dirs
    {
        assemble_directory(String::from(dir), &mut context);
    }

    /* package up all the generated object files into an archive and link against it */
    link_archive(&mut context);
}

/* Turn a binary file into a linkable .o object file.
   the following symbols will be defined pointing to the start and end
   of the object when it is located in memory, and its size in bytes:

    _binary_leafname_start
    _binary_leafname_end
    _binary_leafname_size
   
   where leafname is the leafname of the binary file

   => binary_path = path to binary file to convert
      context    = build context
*/
fn package_binary(binary_path: &String, mut context: &mut Context)
{
    /* generate path to output .o object file for this given binary */
    let leafname = String::from(Path::new(binary_path).file_name().unwrap().to_str().unwrap());
    let object_file = format!("{}/{}.o", &context.output_dir, &leafname);

    /* generate an intemediate .o object file from the given binary file */
    let result = Command::new(&context.ld_exec)
        .arg("-r")
        .arg("--format=binary")
        .arg(&binary_path)
        .arg("-o")
        .arg(&object_file)
        .output()
        .expect(format!("Couldn't run command to convert {} into linkable object file", &binary_path).as_str());

    if result.status.success() != true
    {
        panic!("Conversion of {} to object {} failed:\n{}\n{}",
            &binary_path, &object_file, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap());
    }

    /* when we use ld, it defines the _start, _end, _size symbols using the full filename
    of the binary file, which pollutes the symbol with the architecture and project layout.

    rename the symbols so they can be accessed generically just by their component name.
    we need to convert the '/' and '.' in the path to _ FIXME: this very Unix/Linux-y */
    let symbol_prefix = format!("_binary_{}_", &binary_path.replace("/", "_").replace(".", "_"));
    let renamed_prefix = format!("_binary_{}_", &leafname.replace(".", "_"));

    /* select correct executable */
    let rename = Command::new(&context.oc_exec)
        .arg("--redefine-sym")
        .arg(format!("{}start={}start", &symbol_prefix, &renamed_prefix))
        .arg("--redefine-sym")
        .arg(format!("{}end={}end", &symbol_prefix, &renamed_prefix))
        .arg("--redefine-sym")
        .arg(format!("{}size={}size", &symbol_prefix, &renamed_prefix))
        .arg(&object_file)
        .output()
        .expect(format!("Couldn't run command to rename symbols for {}", &binary_path).as_str());

    if rename.status.success() != true
    {
        panic!("Symbol rename for {} in {} failed:\n{}\n{}",
            &binary_path, &object_file, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap());
    }

    println!("cargo:rerun-if-changed={}", &binary_path);
    register_object(&object_file, &mut context);
}

/* Add an object file, by its full path, to the list of objects to link with.
   To avoid object collisions and overwrites, bail out if the given object path was already taken */
fn register_object(path: &String, context: &mut Context)
{
    if context.objects.insert(path.to_string()) == false
    {
        panic!("Cannot register object {} - an object already exists in that location", &path);
    }
}

/* Run through a directory of .s assembly source code,
   add each .s file to the project, and assemble each file using the appropriate tools
   => slurp_from = path of directory to scan for .s files to assemble
      context = build context
*/
fn assemble_directory(slurp_from: String, context: &mut Context)
{
    /* no longer accept missing directories, though don't fail empty directories */
    let directory = match fs::read_dir(&slurp_from)
    {
        Ok(d) => d,
        Err(e) => panic!("Cannot assembly directory {}: {}", &slurp_from, e)
    };

    for file in directory
    {
        if let Ok(file) = file
        {
            /* assume everything in the asm directory can be assembled if it is a file */
            if let Ok(metadata) = file.metadata()
            {
                if metadata.is_file() == true
                {
                    assemble(file.path().to_str().unwrap(), context);
                }
            }
        }
    }
}

/* Attempt to assemble a given .s source file into a .o object file
   => path = path to .s file to assemble. non-.s files are silently ignored
      context = build context
*/
fn assemble(path: &str, mut context: &mut Context)
{
    /* create name from .s source file's path - extract just the leafname and drop the
    file extension. so extract 'start' from 'src/platform-blah/asm/start.s' */
    let re = Regex::new(r"(([A-Za-z0-9_]+)(/))+(?P<leaf>[A-Za-z0-9_]+)(\.s)").unwrap();
    let matches = re.captures(&path);
    if matches.is_none() == true
    {
        return; /* skip non-conformant files */
    }

    /* extract leafname (sans .s extension) from the path */
    let leafname = &(matches.unwrap())["leaf"];

    /* build pathname for the target .o file */
    let object_file = format!("{}/{}.o", &context.output_dir, &leafname);

    /* now let's try to assemble the .s into an intermediate .o */
    let result = Command::new(&context.as_exec)
        .arg("-march")
        .arg(&context.target.cpu_arch)
        .arg("-mabi")
        .arg(&context.target.abi)
        .arg("--defsym")
        .arg(format!("ptrwidth={}", &context.target.ptr_width))
        .arg("--defsym")
        .arg(format!("fpwidth={}", &context.target.fp_width))
        .arg("-o")
        .arg(&object_file)
        .arg(path)
        .output()
        .expect(format!("Failed to execute command to assemble {}", path).as_str());

    if result.status.success() != true
    {
        panic!("Assembling {} failed:\n{}\n{}",
            &path, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap());
    }

    println!("cargo:rerun-if-changed={}", &path);
    register_object(&object_file, &mut context);
}

/* Create an archive containing all registered .o files and link with this archive */
fn link_archive(context: &mut Context)
{
    let archive_name = String::from("mason-bundle");
    let archive_path = format!("{}/lib{}.a", &context.output_dir, &archive_name);

    /* create archive from .o files in the output directory */
    let mut cmd = Command::new(&context.ar_exec);
    cmd.arg("crus").arg(&archive_path);

    /* add list of object files generated */
    for obj in context.objects.iter()
    {
        cmd.arg(obj);
    }

    /* run command */
    let result = cmd.output().expect(format!("Failed to execute command to archive {}", &archive_path).as_str());

    if result.status.success() != true
    {
        panic!("Archiving {} failed:\n{}\n{}",
            &archive_path, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap());
    }

    /* tell the linker where to find our archive, and ensure anything relying on it is rebuilt as necessary */
    println!("cargo:rustc-link-search={}", &context.output_dir);
    println!("cargo:rustc-link-lib=static={}", &archive_name);
}

/* find, load, and parse a configuration file for this run */
fn parse_config_file() -> Config
{
    let config_location = match search_for_config(CONFIG_FILE)
    {
        Some(p) => p,
        None => fatal_error(format!("Can't find configuration file {:?} in host file system", CONFIG_FILE))
    };

    let config_contents = match fs::read_to_string(&config_location)
    {
        Ok(c) => c,
        Err(e) => fatal_error(format!("Can't read configuration file {:?} in host file system: {}", config_location, e))
    };

    match toml::from_str(config_contents.as_str())
    {
        Ok(c) => c,
        Err(e) => fatal_error(format!("Can't parse configuration file {:?}: {}", config_location, e))
    }
}

/* starting in the current working directory, check for the presence of the
   required config file, and if it's not there, check inside the parent.
   continue up the host file system tree until after hitting the root node.
   this function gives up after SEARCH_MAX iterations to avoid infinite loops.
   => leafname = config file leafname to look for
   <= returns filename of found config file, or None if unsuccessful */
fn search_for_config(leafname: &str) -> Option<PathBuf>
{
    let mut path = match env::current_dir()
    {
        Ok(p) => p,
        Err(e) => fatal_error(format!("Can't get the current working directory ({})", e))
    };

    for _ in 0..SEARCH_MAX
    {
        let mut attempt = path.clone();
        attempt.push(leafname);
        if attempt.exists() == true
        {
            return Some(attempt);
        }

        path = match path.parent()
        {
            Some(p) => p.to_path_buf(),
            None => return None /* give up if we can't go any higher in the tree */
        }
    }

    None
}

/* parse a ConfigEntry structure and add any found file paths to the given arrays
   => entry = ConFigEntry structure to parse
      include_files = table to which 'include_files' string entries will be added
      asm_dirs = table to which 'asm_dirs' string entries will be added
*/
fn add_file_paths_from_config(entry: &ConfigEntry, include_files: &mut HashSet<String>, asm_dirs: &mut HashSet<String>)
{
    match &entry.include_files
    {
        Some(files) => for file in files
        {
            include_files.insert(file.to_string());
        },
        None => ()
    }

    match &entry.asm_dirs
    {
        Some(files) => for file in files
        {
            asm_dirs.insert(file.to_string());
        },
        None => ()
    }
}

/* bail out with an error msg */
fn fatal_error(msg: String) -> !
{
    println!("Mason error: {}", msg);
    exit(1);
}