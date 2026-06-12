// The ARM64 patch mechanism in this file is adapted from the MIT-licensed
// cslol-tools subtree at LeagueToolkit/cslol-manager commit
// 23f230858bc2359ce279e07ed129d482fe3b00bf.

#if defined(__APPLE__) && (defined(__aarch64__) || defined(__arm64__))

#include "macho.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include <libproc.h>
#include <mach/mach.h>
#include <mach/mach_traps.h>
#include <mach/mach_vm.h>
#include <unistd.h>

namespace {

using EventCallback = void (*)(void*, const char*, std::uint32_t, const char*);
using StopCallback = bool (*)(void*);
using PtrStorage = std::uint64_t;

constexpr const char* SIGNATURE_ID = "mac-arm64-pattern-v1";

void set_error(char* buffer, std::size_t buffer_len, std::string_view message) {
    if (!buffer || buffer_len == 0) {
        return;
    }
    const auto count = std::min(buffer_len - 1, message.size());
    std::memcpy(buffer, message.data(), count);
    buffer[count] = '\0';
}

std::filesystem::path canonical_path(const std::filesystem::path& path) {
    std::error_code error;
    auto result = std::filesystem::weakly_canonical(path, error);
    if (error) {
        throw std::runtime_error("Failed to canonicalize path: " + error.message());
    }
    return result;
}

class Process {
public:
    Process() = default;
    Process(mach_port_t task, std::uint32_t pid) : task_(task), pid_(pid) {}
    Process(const Process&) = delete;
    Process& operator=(const Process&) = delete;

    Process(Process&& other) noexcept {
        std::swap(task_, other.task_);
        std::swap(pid_, other.pid_);
        std::swap(base_, other.base_);
    }

    ~Process() {
        if (task_ != MACH_PORT_NULL) {
            mach_port_deallocate(mach_task_self(), task_);
        }
    }

    static std::uint32_t find_pid(const std::filesystem::path& exact_path) {
        std::array<pid_t, 4096> pids{};
        const int bytes =
            proc_listpids(PROC_ALL_PIDS, 0, pids.data(), static_cast<int>(sizeof(pids)));
        if (bytes <= 0) {
            return 0;
        }

        const auto expected = exact_path.generic_string();
        const int count = bytes / static_cast<int>(sizeof(pid_t));
        std::array<char, PROC_PIDPATHINFO_MAXSIZE> path{};
        for (int index = 0; index < count; ++index) {
            if (pids[index] <= 0) {
                continue;
            }
            const int length =
                proc_pidpath(pids[index], path.data(), static_cast<std::uint32_t>(path.size()));
            if (length <= 0) {
                continue;
            }
            if (std::string_view(path.data(), static_cast<std::size_t>(length)) == expected) {
                return static_cast<std::uint32_t>(pids[index]);
            }
        }
        return 0;
    }

    static Process open(std::uint32_t pid) {
        mach_port_t task = MACH_PORT_NULL;
        const auto result = task_for_pid(mach_task_self(), static_cast<int>(pid), &task);
        if (result != KERN_SUCCESS) {
            throw std::runtime_error("task_for_pid failed with code " + std::to_string(result));
        }
        return Process(task, pid);
    }

    bool exited() const {
        std::array<char, PROC_PIDPATHINFO_MAXSIZE> path{};
        return proc_pidpath(static_cast<int>(pid_), path.data(),
                            static_cast<std::uint32_t>(path.size())) <= 0;
    }

    PtrStorage base() {
        if (base_ != 0) {
            return base_;
        }

        vm_map_offset_t offset = 0;
        vm_map_size_t size = 0;
        natural_t depth = 0;
        vm_region_submap_info_data_64_t info{};
        mach_msg_type_number_t count = VM_REGION_SUBMAP_INFO_COUNT_64;
        const auto result = mach_vm_region_recurse(
            task_, &offset, &size, &depth, reinterpret_cast<vm_region_recurse_info_t>(&info),
            &count);
        if (result != KERN_SUCCESS) {
            throw std::runtime_error("mach_vm_region_recurse failed with code " +
                                     std::to_string(result));
        }
        if (offset < 0x100000000ULL) {
            throw std::runtime_error("Unexpected Mach-O image base");
        }
        base_ = offset - 0x100000000ULL;
        return base_;
    }

    void* allocate(std::size_t size) const {
        mach_vm_address_t address = 0;
        const auto result = mach_vm_allocate(task_, &address, size, VM_FLAGS_ANYWHERE);
        if (result != KERN_SUCCESS) {
            throw std::runtime_error("mach_vm_allocate failed with code " +
                                     std::to_string(result));
        }
        return reinterpret_cast<void*>(address);
    }

    void write(void* address, const void* source, std::size_t size) const {
        const auto result = mach_vm_write(
            task_, reinterpret_cast<mach_vm_address_t>(address),
            reinterpret_cast<vm_offset_t>(const_cast<void*>(source)),
            static_cast<mach_msg_type_number_t>(size));
        if (result != KERN_SUCCESS) {
            throw std::runtime_error("mach_vm_write failed with code " +
                                     std::to_string(result));
        }
    }

    void writable(void* address, std::size_t size) const {
        const auto result =
            mach_vm_protect(task_, reinterpret_cast<mach_vm_address_t>(address), size, FALSE,
                            VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY);
        if (result != KERN_SUCCESS) {
            throw std::runtime_error("mach_vm_protect writable failed with code " +
                                     std::to_string(result));
        }
    }

    void executable(void* address, std::size_t size) const {
        const auto result =
            mach_vm_protect(task_, reinterpret_cast<mach_vm_address_t>(address), size, FALSE,
                            VM_PROT_READ | VM_PROT_EXECUTE);
        if (result != KERN_SUCCESS) {
            throw std::runtime_error("mach_vm_protect executable failed with code " +
                                     std::to_string(result));
        }
    }

private:
    mach_port_t task_ = MACH_PORT_NULL;
    std::uint32_t pid_ = 0;
    PtrStorage base_ = 0;
};

struct PayloadFopenHook {
    unsigned char fopen_hook[0x100]{};
    PtrStorage fopen_org_ptr{};
    char prefix[0x100]{};
};

__asm__(R"(
.text

.global _fopen_hook_shellcode_beg
.global _fopen_hook_shellcode_end

.set buffer_size, 0x200

filename .req x19
mode .req x20
filename_len .req x21
fopen_org .req x22

.p2align 8
_fopen_hook_shellcode_beg:
    stp     fp, lr, [sp, #-16]!
    mov     fp, sp
    stp     filename, mode, [sp, #-16]!
    stp     filename_len, fopen_org, [sp, #-16]!
    sub     sp, sp, #buffer_size

    mov     filename, x0
    mov     mode, x1

    adr     fopen_org, Lfopen_org_ref
    ldr     fopen_org, [fopen_org]
    ldr     fopen_org, [fopen_org]

Lcheck_args_not_null:
    cbz     filename, Lcall_with_filename
    cbz     mode, Lcall_with_filename

Lcheck_mode_eq_rb:
    ldrb    w2, [mode]
    cmp     w2, #'r'
    b.ne    Lcall_with_filename
    ldrb    w2, [mode, #1]
    cmp     w2, #'b'
    b.ne    Lcall_with_filename
    ldrb    w2, [mode, #2]
    cbnz    w2, Lcall_with_filename

Lget_filename_length:
    mov     filename_len, #0
    mov     x0, filename
Lget_filename_length_continue:
    ldrb    w2, [x0], #1
    cbz     w2, Lget_filename_length_break
    add     filename_len, filename_len, #1
    cmp     filename_len, 0x80
    b.ge    Lcall_with_filename
    b       Lget_filename_length_continue
Lget_filename_length_break:

Lcheck_suffix:
    cmp     filename_len, #7
    b.lt    Lcall_with_filename
    add     x0, filename, filename_len
    sub     x0, x0, #7
    ldr     x2, [x0]
    movz    x3, #0x632E, lsl #0
    movk    x3, #0x696C, lsl #16
    movk    x3, #0x6E65, lsl #32
    movk    x3, #0x0074, lsl #48
    cmp     x2, x3
    b.ne    Lcall_with_filename

Lwrite_prefix:
    mov     x0, sp
    adr     x1, Lprefix
Lwrite_prefix_continue:
    ldrb    w2, [x1], #1
    strb    w2, [x0], #1
    cbnz    w2, Lwrite_prefix_continue

Lwrite_filename:
    sub     x0, x0, #1
    mov     x1, filename
Lwrite_filename_continue:
    ldrb    w2, [x1], #1
    strb    w2, [x0], #1
    cbnz    w2, Lwrite_filename_continue

Lcall_with_buffer:
    mov     x0, sp
    mov     x1, mode
    blr     fopen_org
    cbnz    x0, Lreturn

Lcall_with_filename:
    mov     x0, filename
    mov     x1, mode
    blr     fopen_org

Lreturn:
    add     sp, sp, #buffer_size
    ldp     filename_len, fopen_org, [sp], #16
    ldp     filename, mode, [sp], #16
    ldp     fp, lr, [sp], #16
    ret

.p2align 8
_fopen_hook_shellcode_end:
Lfopen_org_ref:
    .quad   0x11223344556677
Lprefix:
    .quad   0x11223344556677
)");

extern "C" {
extern unsigned char fopen_hook_shellcode_beg[];
extern unsigned char fopen_hook_shellcode_end[];
}

struct PayloadWadVerify {
    unsigned char return_true[0x8] = {
        0x20, 0x00, 0x80, 0xD2, 0xC0, 0x03, 0x5F, 0xD6,
    };
    PtrStorage fopen_hook_ptr{};
};

struct PayloadImportStub {
    std::uint32_t adrp;
    std::uint32_t ldr;
    std::uint32_t br;

    static PayloadImportStub create(std::uint64_t from, std::uint64_t to) {
        const auto page_diff =
            static_cast<std::int64_t>((to & ~0xFFFULL) - (from & ~0xFFFULL)) >> 12;
        if (page_diff < -0x100000 || page_diff > 0xFFFFF) {
            throw std::runtime_error("fopen import stub offset is out of ARM64 range");
        }
        const auto imm21 = static_cast<std::uint32_t>(page_diff) & 0x1FFFFF;
        const auto immlo = (imm21 & 0x3) << 29;
        const auto immhi = ((imm21 >> 2) & 0x7FFFF) << 5;
        return {
            static_cast<std::uint32_t>(0x90000010 | immhi | immlo),
            static_cast<std::uint32_t>(0xF9400210 | (((to & 0xFFF) >> 3) << 10)),
            0xD61F0200,
        };
    }
};

std::uint64_t find_unique_wad_verify(const std::uint8_t* begin, const std::uint8_t* end,
                                     std::uint64_t text_address) {
    constexpr std::array<std::uint8_t, 8> pattern = {
        0xC3, 0x24, 0x80, 0x52, 0x04, 0x20, 0x80, 0x52,
    };
    std::vector<const std::uint8_t*> matches;
    auto cursor = begin;
    while (cursor < end) {
        const auto match = std::search(cursor, end, pattern.begin(), pattern.end());
        if (match == end) {
            break;
        }
        matches.push_back(match);
        cursor = match + 1;
    }
    if (matches.size() != 1) {
        throw std::runtime_error("wad_verify signature must have exactly one match; found " +
                                 std::to_string(matches.size()));
    }

    const auto* instruction_bytes = matches.front() + pattern.size();
    if (instruction_bytes + sizeof(std::uint32_t) > end) {
        throw std::runtime_error("wad_verify signature is truncated");
    }
    std::uint32_t instruction = 0;
    std::memcpy(&instruction, instruction_bytes, sizeof(instruction));
    const auto opcode = instruction & 0xFC000000;
    if (opcode != 0x94000000 && opcode != 0x14000000) {
        throw std::runtime_error("wad_verify signature has an invalid branch instruction");
    }
    const auto relative = static_cast<std::int32_t>(instruction << 6) >> 6;
    const auto branch_pc =
        text_address + static_cast<std::uint64_t>(instruction_bytes - begin);
    return branch_pc + static_cast<std::int64_t>(relative) * 4;
}

struct ScanResult {
    std::uint64_t wad_verify{};
    std::uint64_t fopen_ptr{};
    std::uint64_t fopen_stub{};
};

ScanResult scan_executable(const std::filesystem::path& executable) {
    std::ifstream file(executable, std::ios::binary);
    if (!file) {
        throw std::runtime_error("Failed to open League executable");
    }
    file.seekg(0, std::ios::end);
    const auto size = file.tellg();
    if (size <= 0) {
        throw std::runtime_error("League executable is empty");
    }
    file.seekg(0, std::ios::beg);
    std::vector<unsigned char> data(static_cast<std::size_t>(size));
    file.read(reinterpret_cast<char*>(data.data()), size);
    if (!file) {
        throw std::runtime_error("Failed to read League executable");
    }

    auto macho = lol::MachO{};
    macho.parse_data_arm64(data.data(), data.size());
    const auto [text_address, text_data, text_size] = macho.find_section("__text");
    if (!text_data || text_size == 0) {
        throw std::runtime_error("Failed to find ARM64 __text section");
    }

    ScanResult result;
    result.wad_verify =
        find_unique_wad_verify(text_data, text_data + text_size, text_address);
    result.fopen_ptr = macho.find_import_ptr("_fopen");
    if (result.fopen_ptr == 0) {
        throw std::runtime_error("Failed to find fopen import pointer");
    }
    result.fopen_stub = macho.find_stub_refs(result.fopen_ptr);
    if (result.fopen_stub == 0) {
        throw std::runtime_error("Failed to find fopen import stub");
    }
    return result;
}

void patch_process(Process& process, const ScanResult& scan,
                   const std::filesystem::path& overlay) {
    auto prefix = canonical_path(overlay).generic_string();
    if (!prefix.ends_with('/')) {
        prefix.push_back('/');
    }
    if (prefix.size() >= sizeof(PayloadFopenHook::prefix)) {
        throw std::runtime_error("Overlay prefix path exceeds 255 bytes");
    }
    if (fopen_hook_shellcode_end - fopen_hook_shellcode_beg !=
        static_cast<std::ptrdiff_t>(sizeof(PayloadFopenHook::fopen_hook))) {
        throw std::runtime_error("ARM64 fopen hook payload has an unexpected size");
    }

    const auto fopen_hook = process.allocate(sizeof(PayloadFopenHook));
    const auto wad_verify = reinterpret_cast<void*>(process.base() + scan.wad_verify);
    const auto fopen_ptr = process.base() + scan.fopen_ptr;
    const auto fopen_stub = reinterpret_cast<void*>(process.base() + scan.fopen_stub);

    PayloadFopenHook fopen_payload{};
    fopen_payload.fopen_org_ptr = fopen_ptr;
    std::memcpy(fopen_payload.fopen_hook, fopen_hook_shellcode_beg,
                sizeof(fopen_payload.fopen_hook));
    std::memcpy(fopen_payload.prefix, prefix.c_str(), prefix.size() + 1);

    PayloadWadVerify verify_payload{};
    verify_payload.fopen_hook_ptr = reinterpret_cast<PtrStorage>(fopen_hook);
    const auto verify_hook_slot =
        reinterpret_cast<PtrStorage>(wad_verify) + offsetof(PayloadWadVerify, fopen_hook_ptr);
    const auto stub_payload = PayloadImportStub::create(
        reinterpret_cast<PtrStorage>(fopen_stub), verify_hook_slot);

    process.writable(fopen_hook, sizeof(fopen_payload));
    process.write(fopen_hook, &fopen_payload, sizeof(fopen_payload));
    process.executable(fopen_hook, sizeof(fopen_payload));

    process.writable(wad_verify, sizeof(verify_payload));
    process.write(wad_verify, &verify_payload, sizeof(verify_payload));
    process.executable(wad_verify, sizeof(verify_payload));

    process.writable(fopen_stub, sizeof(stub_payload));
    process.write(fopen_stub, &stub_payload, sizeof(stub_payload));
    process.executable(fopen_stub, sizeof(stub_payload));
}

bool should_stop(void* context, StopCallback callback) {
    return callback && callback(context);
}

void emit(void* context, EventCallback callback, const char* event, std::uint32_t pid = 0,
          const char* detail = "") {
    if (callback) {
        callback(context, event, pid, detail);
    }
}

}  // namespace

extern "C" int ltk_macos_preflight(const char* game_executable, char* error,
                                    std::size_t error_len) {
    try {
        if (!game_executable || !*game_executable) {
            throw std::runtime_error("Game executable path is empty");
        }
        const auto executable = canonical_path(game_executable);
        if (!std::filesystem::is_regular_file(executable)) {
            throw std::runtime_error("Game executable does not exist");
        }
        (void)scan_executable(executable);
        return 0;
    } catch (const std::exception& exception) {
        set_error(error, error_len, exception.what());
        return -1;
    }
}

extern "C" int ltk_macos_test_find_unique_wad_verify(
    const std::uint8_t* text, std::size_t text_len, std::uint64_t text_address,
    std::uint64_t* result, char* error, std::size_t error_len) {
    try {
        if (!text || !result) {
            throw std::runtime_error("Text bytes and result pointer are required");
        }
        *result = find_unique_wad_verify(text, text + text_len, text_address);
        return 0;
    } catch (const std::exception& exception) {
        set_error(error, error_len, exception.what());
        return -1;
    }
}

extern "C" int ltk_macos_test_parse_arm64_text(
    const std::uint8_t* data, std::size_t data_len, std::uint64_t* address,
    std::size_t* size, char* error, std::size_t error_len) {
    try {
        if (!data || !address || !size) {
            throw std::runtime_error("Mach-O bytes, address, and size are required");
        }
        auto macho = lol::MachO{};
        macho.parse_data_arm64(data, data_len);
        const auto [text_address, text_data, text_size] = macho.find_section("__text");
        if (!text_data || text_size == 0) {
            throw std::runtime_error("Failed to find ARM64 __text fixture section");
        }
        *address = text_address;
        *size = text_size;
        return 0;
    } catch (const std::exception& exception) {
        set_error(error, error_len, exception.what());
        return -1;
    }
}

extern "C" int ltk_macos_run(const char* overlay, const char* game_executable, void* context,
                             EventCallback event_callback, StopCallback stop_callback,
                             char* error, std::size_t error_len) {
    try {
        if (!overlay || !game_executable) {
            throw std::runtime_error("Overlay and game executable paths are required");
        }
        const auto overlay_path = canonical_path(overlay);
        const auto executable_path = canonical_path(game_executable);
        const auto executable_string = executable_path.generic_string();
        if (!executable_string.ends_with(
                "/LeagueofLegends.app/Contents/MacOS/LeagueofLegends")) {
            throw std::runtime_error("Configured executable is not the macOS League game process");
        }

        const auto scan = scan_executable(executable_path);
        emit(context, event_callback, "ready", 0, SIGNATURE_ID);

        while (!should_stop(context, stop_callback)) {
            const auto pid = Process::find_pid(executable_path);
            if (pid == 0) {
                emit(context, event_callback, "waitingForGame");
                for (int count = 0; count < 10 && !should_stop(context, stop_callback); ++count) {
                    std::this_thread::sleep_for(std::chrono::milliseconds(100));
                }
                continue;
            }

            emit(context, event_callback, "gameFound", pid);
            auto process = Process::open(pid);
            emit(context, event_callback, "scanning", pid, SIGNATURE_ID);
            patch_process(process, scan, overlay_path);
            emit(context, event_callback, "patched", pid, SIGNATURE_ID);

            while (!process.exited() && !should_stop(context, stop_callback)) {
                std::this_thread::sleep_for(std::chrono::milliseconds(500));
            }
            if (!should_stop(context, stop_callback)) {
                emit(context, event_callback, "gameExited", pid);
            }
        }
        return 0;
    } catch (const std::exception& exception) {
        set_error(error, error_len, exception.what());
        return -1;
    }
}

#else

#include <algorithm>
#include <cstddef>
#include <cstring>
#include <string_view>

namespace {
void set_error(char* buffer, std::size_t buffer_len, std::string_view message) {
    if (!buffer || buffer_len == 0) {
        return;
    }
    const auto count = std::min(buffer_len - 1, message.size());
    std::memcpy(buffer, message.data(), count);
    buffer[count] = '\0';
}
}  // namespace

extern "C" int ltk_macos_preflight(const char*, char* error, std::size_t error_len) {
    set_error(error, error_len, "helper requires macOS on Apple Silicon");
    return -1;
}

extern "C" int ltk_macos_run(const char*, const char*, void*, void (*)(void*, const char*,
                                                                       unsigned int, const char*),
                             bool (*)(void*), char* error, std::size_t error_len) {
    set_error(error, error_len, "helper requires macOS on Apple Silicon");
    return -1;
}

#endif
