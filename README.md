# Mason

Mason provides a build.rs primarily for [Diosix](https://diosix.org) components. It can automatically assemble low-level assembly code and package up binary objects so that they can be linked with and accessed by high-level code. It looks for two environment variables to tell it where to find code and binaries to process:

| Env variable     | Description |
|------------------|-------------|
| `MASON_ASM_DIRS` | Colon-separated directory pathnames of assembly code to assemble |
| `MASON_FILES`    | Colon-separated pathnames of binary files to package up          |

Assembly files are processed by the directory; binary files are processed individually. Exported symbols in the assembly code can be referenced by high-level code. Binary files will each be exported with the following symbols:

| Symbol                   | Description |
|--------------------------|-------------|
| `_binary_leafname_start` | Memory address of binary file's first byte    |
| `_binary_leafname_end`   | Memory address of first byte after file's end |
| `_binary_leafname_size`  | Size of the file in memory in bytes           |

Substitute `leafname` in the above for the binary file's leafname. A file's leafname must be unique within the build. Mason uses GNU binutils to assemble and archive files, so this must be present for the given build target architecture, which is determined from the environment variable `TARGET` set by Cargo. Here are the supported Cargo targets and the binutils executables expected:

| Cargo target     | Binutils executable |
|------------------|---------------------|
| `riscv32imac-*`  | `riscv32-linux-gnu-*` |
| `riscv64imac-*`  | `riscv64-linux-gnu-*` |
| `riscv64gc-*`    | `riscv64-linux-gnu-*` |

Eg, if you're targeting `riscv64gc-unknown-none-elf`, you'll need binutils' `riscv64-linux-gnu-as`, `riscv64-linux-gnu-ld`, etc, present on your build system.

### Contact and code of conduct <a name="contact"></a>

Please [email](mailto:diosix@tuta.io) project lead Chris Williams if you have any questions or issues to raise, wish to get involved, have source to contribute, or have found a security flaw. You can, of course, submit pull requests or raise issues via GitHub, though please consider disclosing security-related matters privately. Please also observe the Diosix project's [code of conduct](https://diosix.org/docs/conduct.html) if you wish to participate.

### Copyright and license <a name="copyright"></a>

Copyright &copy; Chris Williams, 2020. See [LICENSE](LICENSE) for distribution and use of source code and binaries.
