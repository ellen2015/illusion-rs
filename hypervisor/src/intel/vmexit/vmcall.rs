//! This crate includes functionalities to handle virtual machine (VM) exit events in a hypervisor environment, particularly focusing on VMCALL instructions
//! which are used for hypercalls or VM-to-hypervisor communication.

use {
    crate::{
        error::HypervisorError,
        intel::{
            addresses::PhysicalAddress,
            ept::AccessType,
            events::EventInjection,
            hooks::hook_manager::HookManager,
            vm::Vm,
            vmexit::{
                mtf::{set_monitor_trap_flag, update_guest_interrupt_flag},
                ExitType,
            },
        },
    },
    log::*,
    x86::bits64::paging::PAddr,
};

/// Represents various VMCALL commands that a guest can issue to the hypervisor.
#[repr(u64)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VmcallCommand {
    /// Command to indicate an unknown or unimplemented VMCALL command.
    Unknown = 0,
}

/// Handles a VMCALL VM exit by executing the corresponding action based on the VMCALL command.
///
/// # Parameters
///
/// * `vm`: A mutable reference to the virtual machine instance encountering the VMCALL exit.
///
/// # Returns
///
/// * `Ok(ExitType)`: The continuation exit type after handling the VMCALL, usually indicates that VM execution should continue.
/// * `Err(HypervisorError)`: An error if the VMCALL command is unknown or if there's a failure in handling the command.
///
/// # Errors
///
/// * `HypervisorError::UnknownVmcallCommand`: Returned if the VMCALL command is not recognized.
pub fn handle_vmcall(vm: &mut Vm) -> Result<ExitType, HypervisorError> {
    debug!("Handling VMCALL VM exit...");
    trace!("Register state before handling VM exit: {:?}", vm.guest_registers);

    let vmcall_number = vm.guest_registers.rax;
    trace!("Guest RAX - VMCALL command number: {:#x}", vmcall_number);
    trace!("Guest RIP: {:#x}", vm.guest_registers.rip);

    let guest_function_pa = PAddr::from(PhysicalAddress::pa_from_va(vm.guest_registers.rip));
    trace!("Guest PA: {:#x}", guest_function_pa.as_u64());

    let guest_page_pa = guest_function_pa.align_down_to_base_page();
    trace!("Guest Page PA: {:#x}", guest_page_pa.as_u64());

    let guest_large_page_pa = guest_page_pa.align_down_to_large_page();
    trace!("Guest Large Page PA: {:#x}", guest_large_page_pa.as_u64());

    // Set the current hook to the EPT hook for handling MTF exit
    let exit_type = if let Some(shadow_page_pa) = vm.hook_manager.memory_manager.get_shadow_page_as_ptr(guest_page_pa.as_u64()) {
        trace!("Shadow Page PA: {:#x}", shadow_page_pa);

        trace!("Executing VMCALL hook on shadow page for EPT hook at PA: {:#x} with VA: {:#x}", guest_function_pa, vm.guest_registers.rip);
        // crate::windows::log::log_nt_query_system_information_params(&vm.guest_registers);
        // crate::windows::log::log_nt_create_file_params(&vm.guest_registers);
        // crate::windows::log::log_nt_open_process_params(&vm.guest_registers);
        // crate::windows::log::log_mm_is_address_valid_params(&vm.guest_registers);

        let pre_alloc_pt = vm
            .hook_manager
            .memory_manager
            .get_page_table_as_mut(guest_large_page_pa.as_u64())
            .ok_or(HypervisorError::PageTableNotFound)?;

        // Perform swap_page before the mutable borrow for update_guest_interrupt_flag
        vm.primary_ept
            .swap_page(guest_page_pa.as_u64(), guest_page_pa.as_u64(), AccessType::READ_WRITE_EXECUTE, pre_alloc_pt)?;

        let hook_info = vm
            .hook_manager
            .memory_manager
            .get_hook_info_by_function_pa(guest_page_pa.as_u64(), guest_function_pa.as_u64())
            .ok_or(HypervisorError::HookInfoNotFound)?;

        trace!("Hook info: {:#x?}", hook_info);

        // Calculate the number of instructions in the function to set the MTF counter for restoring overwritten instructions by single-stepping.
        // (NOTE: CHANGE HOOK SIZE IF YOU MOVE THIS INTO CPUID OR INT3)
        let instruction_count =
            unsafe { HookManager::calculate_instruction_count(guest_function_pa.as_u64(), HookManager::hook_size(hook_info.ept_hook_type)) as u64 };
        vm.hook_manager.mtf_counter = Some(instruction_count);

        // Set the monitor trap flag and initialize counter to the number of overwritten instructions
        set_monitor_trap_flag(true);

        // Ensure all data mutations to vm are done before calling this.
        // This function will update the guest interrupt flag to prevent interrupts while single-stepping
        update_guest_interrupt_flag(vm, false)?;

        Ok(ExitType::Continue)
    } else {
        EventInjection::vmentry_inject_gp(0);
        Ok(ExitType::Continue)
    };

    exit_type
}
