//! Legalize instructions.
//!
//! A legal instruction is one that can be mapped directly to a machine code instruction for the
//! target ISA. The `legalize_function()` function takes as input any function and transforms it
//! into an equivalent function using only legal instructions.
//!
//! The characteristics of legal instructions depend on the target ISA, so any given instruction
//! can be legal for one ISA and illegal for another.
//!
//! Besides transforming instructions, the legalizer also fills out the `function.encodings` map
//! which provides a legal encoding recipe for every instruction.
//!
//! The legalizer does not deal with register allocation constraints. These constraints are derived
//! from the encoding recipes, and solved later by the register allocator.

use dominator_tree::DominatorTree;
use flowgraph::ControlFlowGraph;
use ir::{Function, Cursor, DataFlowGraph, InstructionData, Opcode, InstBuilder};
use ir::condcodes::IntCC;
use isa::{TargetIsa, Legalize};
use bitset::BitSet;
use ir::instructions::ValueTypeSet;

mod boundary;
mod split;

/// Legalize `func` for `isa`.
///
/// - Transform any instructions that don't have a legal representation in `isa`.
/// - Fill out `func.encodings`.
///
pub fn legalize_function(func: &mut Function,
                         cfg: &mut ControlFlowGraph,
                         domtree: &DominatorTree,
                         isa: &TargetIsa) {
    boundary::legalize_signatures(func, isa);

    func.encodings.resize(func.dfg.num_insts());

    let mut pos = Cursor::new(&mut func.layout);

    // Process EBBs in a reverse post-order. This minimizes the number of split instructions we
    // need.
    for &ebb in domtree.cfg_postorder().iter().rev() {
        pos.goto_top(ebb);

        // Keep track of the cursor position before the instruction being processed, so we can
        // double back when replacing instructions.
        let mut prev_pos = pos.position();

        while let Some(inst) = pos.next_inst() {
            let opcode = func.dfg[inst].opcode();

            // Check for ABI boundaries that need to be converted to the legalized signature.
            if opcode.is_call() && boundary::handle_call_abi(&mut func.dfg, cfg, &mut pos) {
                // Go back and legalize the inserted argument conversion instructions.
                pos.set_position(prev_pos);
                continue;
            }

            if opcode.is_return() &&
               boundary::handle_return_abi(&mut func.dfg, cfg, &mut pos, &func.signature) {
                // Go back and legalize the inserted return value conversion instructions.
                pos.set_position(prev_pos);
                continue;
            }

            if opcode.is_branch() {
                split::simplify_branch_arguments(&mut func.dfg, inst);
            }

            match isa.encode(&func.dfg, &func.dfg[inst], func.dfg.ctrl_typevar(inst)) {
                Ok(encoding) => *func.encodings.ensure(inst) = encoding,
                Err(action) => {
                    // We should transform the instruction into legal equivalents.
                    // Possible strategies are:
                    // 1. Legalize::Expand: Expand instruction into sequence of legal instructions.
                    //    Possibly iteratively. ()
                    // 2. Legalize::Narrow: Split the controlling type variable into high and low
                    //    parts. This applies both to SIMD vector types which can be halved and to
                    //    integer types such as `i64` used on a 32-bit ISA. ().
                    // 3. TODO: Promote the controlling type variable to a larger type. This
                    //    typically means expressing `i8` and `i16` arithmetic in terms if `i32`
                    //    operations on RISC targets. (It may or may not be beneficial to promote
                    //    small vector types versus splitting them.)
                    // 4. TODO: Convert to library calls. For example, floating point operations on
                    //    an ISA with no IEEE 754 support.
                    let changed = match action {
                        Legalize::Expand => expand(&mut func.dfg, cfg, &mut pos),
                        Legalize::Narrow => narrow(&mut func.dfg, cfg, &mut pos),
                    };
                    // If the current instruction was replaced, we need to double back and revisit
                    // the expanded sequence. This is both to assign encodings and possible to
                    // expand further.
                    // There's a risk of infinite looping here if the legalization patterns are
                    // unsound. Should we attempt to detect that?
                    if changed {
                        pos.set_position(prev_pos);
                        continue;
                    }
                }
            }

            // Remember this position in case we need to double back.
            prev_pos = pos.position();
        }
    }
    func.encodings.resize(func.dfg.num_insts());
}

// Include legalization patterns that were generated by `gen_legalizer.py` from the `XForms` in
// `meta/cretonne/legalize.py`.
//
// Concretely, this defines private functions `narrow()`, and `expand()`.
include!(concat!(env!("OUT_DIR"), "/legalizer.rs"));
