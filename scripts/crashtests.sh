# Deliberate crash tests: name -> text the crash report must contain.
# Sourced by build-iso.sh (to make one ISO each) and kernel-tests.sh (to check them).
# The kernel side lives in kernel/src/crashtest.rs.
declare -A CRASHTESTS=(
    [divide]="EXCEPTION #DE divide error"
    [invalid_opcode]="EXCEPTION #UD invalid opcode"
    [breakpoint_then_halt]="breakpoint returned, count 1"
    [general_protection]="EXCEPTION #GP general protection fault"
    [page_fault_read]="page not present on read in kernel mode"
    [page_fault_write]="page not present on write in kernel mode"
    [null_deref]="page fault at 0x0000000000000000"
    [write_rodata]="protection violation on write in kernel mode"
    [execute_data]="protection violation on instruction fetch in kernel mode"
    [double_fault]="EXCEPTION #DF double fault"
    [panic]="KERNEL PANIC: panicked at"
    [stack_overflow]="kernel stack overflow: the fault is in the boot stack's guard page"
    [write_relro]="protection violation on write in kernel mode"
    [write_text]="protection violation on write in kernel mode"
    [execute_direct_map]="protection violation on instruction fetch in kernel mode"
)
