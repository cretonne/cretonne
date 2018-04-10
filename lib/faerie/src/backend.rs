//! Defines `FaerieBackend`.

use container;
use cretonne::binemit::{Addend, CodeOffset, Reloc, RelocSink, TrapSink};
use cretonne::isa::TargetIsa;
use cretonne::result::CtonError;
use cretonne::{self, binemit, ir};
use cton_module::{Backend, DataContext, Linkage, ModuleNamespace};
use faerie;
use failure::Error;
use std::fs::File;
use target;

pub struct FaerieCompiledFunction {}

pub struct FaerieCompiledData {}

/// A `FaerieBackend` implements `Backend` and emits ".o" files using the `faerie` library.
pub struct FaerieBackend<'isa> {
    isa: &'isa TargetIsa,
    artifact: faerie::Artifact,
    format: container::Format,
}

impl<'isa> FaerieBackend<'isa> {
    /// Create a new `FaerieBackend` using the given Cretonne target.
    pub fn new(
        isa: &'isa TargetIsa,
        name: String,
        format: container::Format,
    ) -> Result<Self, Error> {
        debug_assert!(isa.flags().is_pic(), "faerie requires PIC");
        Ok(Self {
            isa,
            artifact: faerie::Artifact::new(target::translate(isa)?, name),
            format,
        })
    }

    /// Call `emit` on the faerie `Artifact`, producing bytes in memory.
    pub fn emit(&self) -> Result<Vec<u8>, Error> {
        match self.format {
            container::Format::ELF => self.artifact.emit::<faerie::Elf>(),
            container::Format::MachO => self.artifact.emit::<faerie::Mach>(),
        }
    }

    /// Call `write` on the faerie `Artifact`, writing to a file.
    pub fn write(&self, sink: File) -> Result<(), Error> {
        match self.format {
            container::Format::ELF => self.artifact.write::<faerie::Elf>(sink),
            container::Format::MachO => self.artifact.write::<faerie::Mach>(sink),
        }
    }
}

impl<'isa> Backend for FaerieBackend<'isa> {
    type CompiledFunction = FaerieCompiledFunction;
    type CompiledData = FaerieCompiledData;

    // There's no need to return invidual artifacts; we're writing them into
    // the output file instead.
    type FinalizedFunction = ();
    type FinalizedData = ();

    fn isa(&self) -> &TargetIsa {
        self.isa
    }

    fn declare_function(&mut self, name: &str, linkage: Linkage) {
        self.artifact
            .declare(name, translate_function_linkage(linkage))
            .expect("inconsistent declarations");
    }

    fn declare_data(&mut self, name: &str, linkage: Linkage, writable: bool) {
        self.artifact
            .declare(name, translate_data_linkage(linkage, writable))
            .expect("inconsistent declarations");
    }

    fn define_function(
        &mut self,
        name: &str,
        ctx: &cretonne::Context,
        namespace: &ModuleNamespace<Self>,
        code_size: u32,
    ) -> Result<FaerieCompiledFunction, CtonError> {
        let mut code: Vec<u8> = Vec::with_capacity(code_size as usize);
        code.resize(code_size as usize, 0);

        // Non-lexical lifetimes would obviate the braces here.
        {
            let mut reloc_sink = FaerieRelocSink {
                format: self.format,
                artifact: &mut self.artifact,
                name,
                namespace,
            };
            let mut trap_sink = FaerieTrapSink {};

            ctx.emit_to_memory(code.as_mut_ptr(), &mut reloc_sink, &mut trap_sink, self.isa);
        }

        self.artifact.define(name, code).expect(
            "inconsistent declaration",
        );
        Ok(FaerieCompiledFunction {})
    }

    fn define_data(&mut self, _name: &str, _data: &DataContext) -> FaerieCompiledData {
        unimplemented!()
    }

    fn write_data_funcaddr(
        &mut self,
        _data: &mut FaerieCompiledData,
        _offset: usize,
        _what: ir::FuncRef,
    ) {
        unimplemented!()
    }

    fn write_data_dataaddr(
        &mut self,
        _data: &mut FaerieCompiledData,
        _offset: usize,
        _what: ir::GlobalVar,
        _usize: binemit::Addend,
    ) {
        unimplemented!()
    }

    fn finalize_function(
        &mut self,
        _func: &FaerieCompiledFunction,
        _namespace: &ModuleNamespace<Self>,
    ) {
        // Nothing to do.
    }

    fn finalize_data(&mut self, _data: &FaerieCompiledData, _namespace: &ModuleNamespace<Self>) {
        // Nothing to do.
    }
}

fn translate_function_linkage(linkage: Linkage) -> faerie::Decl {
    match linkage {
        Linkage::Import => faerie::Decl::FunctionImport,
        Linkage::Local => faerie::Decl::Function { global: false },
        Linkage::Export => faerie::Decl::Function { global: true },
    }
}

fn translate_data_linkage(linkage: Linkage, writable: bool) -> faerie::Decl {
    match linkage {
        Linkage::Import => faerie::Decl::DataImport,
        Linkage::Local => faerie::Decl::Data {
            global: false,
            writable,
        },
        Linkage::Export => faerie::Decl::Data {
            global: true,
            writable,
        },
    }
}

struct FaerieRelocSink<'a, 'isa: 'a> {
    format: container::Format,
    artifact: &'a mut faerie::Artifact,
    name: &'a str,
    namespace: &'a ModuleNamespace<'a, FaerieBackend<'isa>>,
}

impl<'a, 'isa> RelocSink for FaerieRelocSink<'a, 'isa> {
    fn reloc_ebb(&mut self, _offset: CodeOffset, _reloc: Reloc, _ebb_offset: CodeOffset) {
        unimplemented!();
    }

    fn reloc_external(
        &mut self,
        offset: CodeOffset,
        reloc: Reloc,
        name: &ir::ExternalName,
        addend: Addend,
    ) {
        let ref_name = &self.namespace.get_function_decl(name).name;
        let addend_i32 = addend as i32;
        let raw_reloc = container::raw_relocation(reloc, self.format);
        debug_assert!(addend_i32 as i64 == addend);
        self.artifact
            .link_with(
                faerie::Link {
                    from: self.name,
                    to: ref_name,
                    at: offset as usize,
                },
                faerie::RelocOverride {
                    reloc: raw_reloc,
                    addend: addend_i32,
                },
            )
            .expect("faerie relocation error");
    }

    fn reloc_jt(&mut self, _offset: CodeOffset, _reloc: Reloc, _jt: ir::JumpTable) {
        unimplemented!();
    }
}

struct FaerieTrapSink {}

impl TrapSink for FaerieTrapSink {
    // Ignore traps for now. For now, frontends should just avoid generating code that traps.
    fn trap(&mut self, _offset: CodeOffset, _srcloc: ir::SourceLoc, _code: ir::TrapCode) {}
}
