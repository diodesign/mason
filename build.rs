/* Assemble low-level assembly code and package up binaries for linking with higher-level code
 *
 * This code uses the --target passed to cargo build to determine the tools it needs.
 * It assumes GNU binutils are present on the host system to assemble code and package binaries.
 * It also uses the following environment variables to find files to process:
 * 
 * MASON_ASM_DIRS = Colon-separated directory pathnames of assembly code to assemble
 * MASON_FILES = Colon-separated pathnames of binary files to package up
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
use std::path::Path;
use std::process::Command;
use std::collections::HashSet;

extern crate regex;
use regex::Regex;

/* describe a build target from its user-supplied triple */
struct Target
{
    pub cpu_arch: String,    /* define the CPU architecture to generate code for */
    pub gnu_prefix: String,  /* locate the GNU as and ar tools */ 
    pub platform: String,    /* locate the tail of the platform directory in src, eg riscv for src/platform-riscv */
    pub width: usize,        /* pointer width in bits */
    pub abi: String          /* define the ABI for this target */
}

impl Target
{
    /* create a target object from a full build triple string, taking the CPU arch from the first part of the triple  */
    pub fn new(triple: String) -> Target
    {
        match triple.split('-').next().expect("Badly formatted target triple").as_ref()
        {
            "riscv32imac" => Target
            {
                cpu_arch: String::from("rv32imac"),
                gnu_prefix: String::from("riscv32"),
                platform: String::from("riscv"),
                width: 32,
                abi: String::from("ilp32")
            },
            "riscv64imac" => Target
            {
                cpu_arch: String::from("rv64imac"),
                gnu_prefix: String::from("riscv64"),
                platform: String::from("riscv"),
                width: 64,
                abi: String::from("lp64")
            },
            "riscv64gc" => Target
            {
                cpu_arch: String::from("rv64gc"),
                gnu_prefix: String::from("riscv64"),
                platform: String::from("riscv"),
                width: 64,
                abi: String::from("lp64")
            },
            unknown_target => panic!("Unsupported target '{}'", &unknown_target)
        }
    }
}

/* shared context of this build run */
pub struct Context<'a>
{
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
    let target = Target::new(env::var("TARGET").expect("Missing target triple, use --target with cargo"));

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

    /* package up individual binary files */
    if let Some(files) = env::var_os("MASON_FILES")
    {
        for f in files.into_string().unwrap().split(":")
        {
            package_binary(&String::from(f), &mut context);
        }
    }

    /* assemble all asm code in each of these directories */
    if let Some(asm_dir) = env::var_os("MASON_ASM_DIRS")
    {
        for dir in asm_dir.into_string().unwrap().split(":")
        {
            assemble_directory(String::from(dir), &mut context);
        }
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
        panic!(format!("Conversion of {} to object {} failed:\n{}\n{}",
            &binary_path, &object_file, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap()));
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
        panic!(format!("Symbol rename for {} in {} failed:\n{}\n{}",
            &binary_path, &object_file, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap()));
    }

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
                    println!(
                        "cargo:rerun-if-changed={}",
                        file.path().to_str().unwrap()
                    );
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
    let re = Regex::new(r"(([A-Za-z0-9_]+)(/))+(?P<leaf>[A-Za-z0-9]+)(\.s)").unwrap();
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
        .arg(format!("ptrwidth={}", &context.target.width))
        .arg("-o")
        .arg(&object_file)
        .arg(path)
        .output()
        .expect(format!("Failed to execute command to assemble {}", path).as_str());

    if result.status.success() != true
    {
        panic!(format!("Assembling {} failed:\n{}\n{}",
            &path, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap()));
    }

    register_object(&object_file, &mut context);
}

/* Create an archive containing all registered .o files and link with this archive */
fn link_archive(context: &mut Context)
{
    let archive_name = String::from("hv");
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
        panic!(format!("Archiving {} failed:\n{}\n{}",
            &archive_path, String::from_utf8(result.stdout).unwrap(), String::from_utf8(result.stderr).unwrap()));
    }

    /* tell the linker where to find our archive */
    println!("cargo:rustc-link-search={}", &context.output_dir);
    println!("cargo:rustc-link-lib=static={}", &archive_name);
}
