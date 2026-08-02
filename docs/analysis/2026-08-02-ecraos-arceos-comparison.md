# ecraOS 与 ArceOS 的阶段性对比

Date: 2026-08-02

## 1. 文档范围

本文比较以下两个源码快照：

- ecraOS：提交 `ea7453f`（2026-07-29，`feat(riscv): add supervisor timer interrupts`）。
- TGOSKits：提交 `31f341abc`（2026-07-31，`fix(cpu-local): guard uninstalled host CPU
  areas (#1798)`），其中主要参考对象是 `refs/tgoskits/os/arceos`。

这里的“ArceOS”特指 TGOSKits 中的 ArceOS 核心，即由 `axruntime`、`axhal`、`axtask`、
`axmm`、`axfs-ng`、`ax-net`、`ax-driver`、`axstd` 和相关 API crate 组成的模块化
unikernel/runtime。TGOSKits 还包含两类更上层的系统，它们不应直接计入 ArceOS 核心：

- StarryOS 在 ArceOS 组件之上实现 Linux ELF 用户进程、系统调用和信号等兼容层。
- AxVisor 是独立的虚拟机监控器。

因此，本文在讨论“Linux 兼容”时会明确写成 StarryOS 的能力，不把它误写成 ArceOS
核心已经通过陷阱入口提供完整 Linux 系统调用 ABI。

本文使用三种状态描述实现成熟度：

- **源码存在**：仓库中已经有相应类型、函数或模块，但不保证主启动路径会调用它。
- **已接入**：相应模块已经进入内核或运行时的正常初始化、调用路径。
- **实测通过**：除源码审阅外，本次审计还实际构建并运行了对应路径。

## 2. 核心结论

ecraOS 已经实现了一个结构清晰、能够在 x86-64 和 RISC-V 64 上启动的内核基础层。
它已经覆盖引导加载、PIE 内核重定位、高半地址空间、页表、物理页和堆分配、SMP
启动、每 CPU 数据、基础定时器以及关机路径。其中 x86-64 的多核路径和 RISC-V 64
的 release 多核路径均在本次审计中实际运行通过。

但 ecraOS 目前还没有越过“内核基础设施”到“可承载应用的 unikernel/OS runtime”
这一分界线。它没有任务模型、调度器、上下文切换、应用入口、文件系统、网络栈和
完整设备驱动，也没有完整的外部中断、IPI 和跨 CPU TLB shootdown。当前启动后的
主要工作仍是分配器、跨 CPU 释放和定时器的自检，随后关机。

相比之下，ArceOS 已经是一套可组合的应用运行环境。它从 `axruntime` 统一初始化
内存、分页、任务、中断、设备、文件系统和网络，并能通过 `axstd`、`axlibc`、
`arceos_api` 和 `arceos_posix_api` 承载 Rust 或 C 应用。其任务、驱动、文件系统、
网络和测试体系显著超过 ecraOS 当前阶段。

因此，二者当前并不是同一成熟度层级上的等价实现：

> ecraOS 已经形成较扎实且便于教学追踪的启动、地址空间、分配器和 SMP 基础；
> ArceOS 已经在这些基础之上形成完整的模块化 unikernel 应用运行时。

## 3. 能力矩阵

| 领域 | ecraOS 当前状态 | ArceOS 当前状态 | 判断 |
| --- | --- | --- | --- |
| 引导与加载 | x86-64 Multiboot 1 和 RISC-V loader，加载器与 PIE 内核分离 | 多平台启动层和统一 runtime | ecraOS 已形成完整且清晰的引导链 |
| 架构支持 | x86-64、RISC-V 64 | x86-64、RISC-V 64、AArch64、LoongArch64 | ecraOS 架构覆盖较窄 |
| 虚拟内存 | 高半内核、运行时页表模式选择、映射/拆分/回收、`vmalloc` | 内核/用户地址空间、缺页处理、TLB shootdown | ecraOS 内核 VMM 较强，完整地址空间生命周期仍欠缺 |
| 物理内存与堆 | early allocator、buddy、每 CPU slab、远程释放、`vmalloc` | buddy/slab/TLSF 等可选分配器和 runtime 接入 | ecraOS 已有可信的分层实现 |
| SMP | BSP/AP 或 hart 启动、每 CPU 数据、跨 CPU slab 释放自检 | 启动、IPI、调度、亲和性、同步、TLB shootdown | ecraOS 只完成 bring-up 基础 |
| 定时器 | x86 LAPIC timer，RISC-V supervisor timer | 时钟、定时器、中断、睡眠和调度集成 | ecraOS 已接入硬件，但未形成调度语义 |
| 中断 | RISC-V timer 可用；x86 通用 IRQ 注册未实现 | IRQ 管理、平台控制器、驱动和任务协作 | ecraOS 是关键短板 |
| 任务与调度 | 未实现 | FIFO、RR、CFS，可选抢占和多任务 | ecraOS 尚无应用执行模型 |
| 用户态 | 未实现 | ArceOS 可选用户地址空间；StarryOS 提供 Linux 用户进程层 | 不应把 StarryOS 能力算作 ArceOS 核心 syscall ABI |
| 设备与驱动 | ACPI/DT CPU 枚举和少量平台初始化，无完整设备模型 | PCI、块、网络、显示、输入、串口、RTC、USB 等 | ecraOS 主要停留在发现和平台初始化 |
| 文件系统 | 未实现 | VFS、页缓存、FAT、ext4、分区和根文件系统选择 | ecraOS 未实现 |
| 网络 | 未实现 | 基于 smoltcp 的 IPv4/IPv6、TCP、UDP、ICMP、DHCP、DNS 等 | ecraOS 未实现 |
| 应用接口 | 无应用入口和标准库层 | `axstd`、`axlibc`、ArceOS API、POSIX API | ArceOS 已能承载应用 |
| 测试与 CI | 库单元测试和 QEMU 启动脚本 | 大量 Rust/C 应用测试、平台测试和 CI | ecraOS 的系统级自动判定不足 |

## 4. ecraOS 已经实现的部分

### 4.1 加载器与 PIE 内核边界

ecraOS 将加载器和内核拆成两个职责明确的阶段：

1. `ecraldr-*` 负责目标平台的启动协议和初始环境。
2. `ecraldr_base` 定义启动参数和内核入口胶水。
3. 加载器嵌入 strip 后的内核二进制，将其装载后进入 PIE 内核。
4. `ecraos` 在自己的高半地址空间中继续初始化。

x86-64 当前使用 Multiboot 1 loader，RISC-V 64 使用对应的 SBI/QEMU loader。架构支持
代码 `exarch` 已经作为正常 workspace 依赖进入内核 Cargo graph，不再通过额外的
平台实现制品拼接。

这一部分不是只有孤立源码。本次审计实际通过了 x86-64 debug 启动和 RISC-V 64
release 启动。

### 4.2 高半地址空间和页表

内核入口在完成必要的早期准备后切换到高半地址空间。VMM 会根据架构支持能力选择
最大的统一虚拟地址模式，而不是把所有代码固定在单一页表层级上。初始化路径还会
移除早期恒等映射，降低长期保留低地址别名的风险。

独立的 `expt` crate 已经提供：

- 类型化的虚拟地址、物理地址和地址范围。
- 页表创建、遍历、映射、查询和取消映射。
- 大页及其拆分处理。
- TLB 刷新批处理接口。
- 页表相关单元测试。

内核侧的 `vmalloc` 进一步在页表之上提供虚拟区间管理、guard page、外部物理区间
映射、反向翻译和取消映射。对一个教学内核而言，这比只有“启动时建立几张静态
页表”更完整，也为内核栈和后续大对象映射提供了统一机制。

不过，目前尚未看到 ArceOS `axmm` 所覆盖的完整内核/用户地址空间抽象、用户映射
生命周期、通用缺页处理，以及 SMP 下跨 CPU TLB shootdown。

### 4.3 分层内存分配器

ecraOS 的内存分配路径已经形成清楚的层次：

- early allocator 负责正式内存管理建立前的早期分配。
- `exbuddy` 管理物理页或较大粒度的连续内存。
- `exslab` 提供小对象 slab 分配。
- 每 CPU slab 降低常用小对象分配的共享锁竞争。
- remote-free 路径允许对象在其他 CPU 上释放并归还其所属 slab。
- `vmalloc` 为非物理连续的虚拟内存和带 guard page 的区域提供支持。

x86-64 四 CPU 和 RISC-V 64 双 hart 的启动日志均显示 remote-free 自检成功。这说明
跨 CPU 释放不是只存在于接口层，而是至少经过当前 QEMU 场景的实际执行。

仍有一些未收尾内容。例如内存初始化中保留了回收 loader/boot stack 占用空间的
TODO，说明可用物理内存的生命周期还没有完全闭合。

### 4.4 SMP 启动和每 CPU 数据

ecraOS 能够枚举 CPU，建立逻辑 CPU 编号与硬件 ID 的映射，为 AP 分配带 guard page
的栈，并启动 x86-64 AP 或 RISC-V hart。每 CPU 基础设施已经用于 slab allocator 和
定时器状态，而不只是保存一个 CPU ID。

本次实际运行结果包括：

- x86-64 debug，单 CPU：完整启动、定时器运行并关机。
- x86-64 debug，四 CPU：三个 AP 启动、remote-free 自检、各 CPU 定时器运行并关机。
- RISC-V 64 release，单 hart：定时器累计到 100 次后关机。
- RISC-V 64 release，双 hart：第二个 hart 启动、remote-free 自检、两 hart 定时器运行
  并关机。

但当前 SMP 主要等于“把 CPU 启起来”。它还没有任务迁移、CPU affinity、通用 IPI、
跨 CPU reschedule、TLB shootdown 或 CPU offline。AP 启动等待路径也缺少超时和明确
错误恢复，在固件或硬件拒绝启动时可能永久等待。

### 4.5 平台发现和定时器

x86-64 路径能够使用 ACPI 枚举处理器和中断控制器信息，RISC-V 路径能够从设备树
提取 CPU/hart 信息。x86-64 已有 LAPIC/x2APIC 和本地定时器初始化，RISC-V 64 已有
supervisor timer 中断注册与续期。

当前定时器更接近硬件与中断路径的健康检查。由于没有任务和 wait queue，它还没有
支撑 `sleep`、调度时间片、超时唤醒等 OS 级时间语义。

## 5. ecraOS 尚未实现或尚未完成的部分

### 5.1 任务、调度和应用入口

这是与 ArceOS 差距最大的部分。ecraOS 当前没有：

- task/thread 数据结构和生命周期。
- 上下文切换。
- 就绪队列和调度策略。
- 抢占、阻塞、唤醒、等待队列和睡眠。
- 内核应用的统一入口。
- Rust 标准库替代层、C library 或 POSIX API。

当前 BSP 和 AP 入口完成初始化后主要执行 allocator、remote-free、timer 等自检。
ArceOS 的 `axruntime` 则会根据 feature 依次初始化 allocator、paging、platform、task、
IRQ、devices、filesystem 和 network，最后调用应用主函数。这个差异决定了 ecraOS
现在还不能承载与 ArceOS 示例应用同类的工作负载。

### 5.2 完整中断、IPI 和异常处理

x86-64 的通用 IRQ 注册、注销和分发接口仍返回失败，IOAPIC 相关接入也没有完成。
这意味着本地 APIC timer 可工作，并不代表平台外部中断已经形成可供驱动使用的
完整路径。

RISC-V 64 已经实现 supervisor timer，但 software interrupt 和 external interrupt
仍明确未实现或被屏蔽。因而还没有 PLIC 外部设备中断和可用于 SMP 协作的 software
IPI。

页表层有 TLB flush 抽象，但系统层还没有跨 CPU shootdown。通用 page fault 的默认
处理也尚未形成像 ArceOS `axmm` 那样可处理地址空间缺页、按需映射或用户访问错误的
路径。

### 5.3 设备模型和驱动

ecraOS 已有 ACPI/DT 探测和 CPU 枚举，但尚未形成统一、可扩展的设备/驱动体系。
`exarch` 中部分 `DeviceIf` 仍未实现，内核目前直接使用自己的探测路径绕过这些接口。
这会让“平台抽象”和“内核实际使用的接口”发生分叉。

尚未看到完整接入的：

- PCI 枚举、BAR 和 MSI/MSI-X 管理。
- 块设备和磁盘控制器。
- 网络设备。
- 输入、显示、USB、RTC 等常见设备类别。
- 驱动注册、匹配、初始化和资源管理框架。

ArceOS/TGOSKits 已有对应的 driver crates 和 runtime 接入。不过应注意，“crate 和
feature 存在”不自动等于每个驱动在每个平台都经过端到端验证。

### 5.4 文件系统和网络

ecraOS 尚无 VFS、页缓存、块缓存、文件描述符、挂载、FAT/ext4 或根文件系统选择，
也没有网络设备抽象和协议栈。

ArceOS 已有 `axfs-ng` 及相关 VFS 组件，可组合 FAT、ext4、分区识别和页缓存；
`ax-net` 基于 smoltcp 提供 IPv4/IPv6、ICMP、UDP、TCP、DHCP 和 DNS 等能力，并有
loopback 和网络应用测试。这些模块也已经进入 `axruntime` 的 feature 化初始化路径。

### 5.5 用户态和 Linux 兼容

ecraOS 当前没有用户地址空间、ELF 用户程序加载、用户/内核权限切换、系统调用 ABI、
信号或进程资源模型。

ArceOS 核心有可选的 user-space 地址空间支持和 POSIX 风格静态 API，但普通 ArceOS
unikernel 应用中的 POSIX 调用通常是库到内核模块的直接调用，不等于通过 syscall
trap 运行不可信 Linux 进程。TGOSKits 中真正的 Linux ELF、trap/syscall dispatch 和
用户任务逻辑位于 StarryOS，例如 `kernel/src/entry.rs`、`task/user.rs` 和
`syscall/mod.rs`。

### 5.6 架构和平台覆盖

ecraOS 的目标方向包含 x86-64 和 RISC-V 64，目前这两个目标都已经有可运行路径。
ArceOS/TGOSKits 则还覆盖 AArch64 和 LoongArch64，并具有更多机器和动态平台组合。

此外，ecraOS 中某些架构接口仍留有 TODO，例如 x86-64 和 RISC-V 64 power 接口里的
`current_cpu_id`。另有 opaque page-table cursor 被条件编译关闭，说明抽象整理仍在
推进中。

## 6. ecraOS 已实现部分相对 ArceOS 的优点

### 6.1 引导边界更直观

loader、boot argument、PIE kernel 和 architecture support 的边界明确。沿着
`ecraldr-* -> ecraldr_base -> ecraos -> exarch` 阅读，可以较容易理解每个阶段由谁
建立执行环境。对研究装载地址、重定位和高半切换而言，这种结构很适合教学。

### 6.2 地址语义更显式

ecraOS 广泛使用 `VirtAddr`、`PhysAddr`、`VirtAddrRange` 和 `PhysAddrRange` 等语义
类型，并将整数转换尽量压到指针、汇编和原子存储边界。这能减少把物理地址、虚拟
地址、长度和偏移混为 `usize` 的错误，也让内存管理代码的意图更容易审查。

### 6.3 VMM 和分配器路径便于逐层学习

运行时页表模式选择、大页拆分、TLB flush 批处理、`vmalloc` guard page，以及
early/buddy/slab/per-CPU/remote-free 的层次都较明确。ArceOS 对应能力更完整，但功能
被拆散在更多 crate、feature、宏和平台接口中，初次阅读需要建立更大的全局模型。

### 6.4 较少的 feature 组合和间接层

ecraOS 当前系统较小，主路径集中，能够从入口一直追踪到页表、分配器、AP 启动和
定时器。ArceOS 的高可配置性适合复用，但也意味着同一函数在不同 feature 和平台下
可能走向不同实现，阅读和验证某一具体配置的成本更高。

### 6.5 基础组件有独立复用价值

`expt`、`exbuddy`、`exslab`、`expercpu` 和 `memory_range_set` 等 crate 并非只能依附
于一个巨型内核入口，它们有相对聚焦的接口和单元测试。当前选定 host-side crate
测试共通过 104 个单元/集成测试和 29 个 doctest。

## 7. ecraOS 已实现部分相对 ArceOS 的不足

### 7.1 很多基础设施还没有真实消费者

页表、`vmalloc`、每 CPU allocator 和定时器本身已经有相当深度，但缺少 task、driver、
filesystem 和 network 作为持续消费者。结果是很多行为只在启动自检中被覆盖，还未
承受长期运行、资源回收、并发取消、错误注入和复杂依赖顺序。

### 7.2 SMP 能启动，但不能协调完整 OS 工作负载

remote-free 证明了跨 CPU 共享状态已经开始工作，但没有 IPI、调度、任务迁移和 TLB
shootdown，SMP 仍停留在 bring-up 层。ArceOS 的多核测试已经覆盖 affinity、IPI、
并行任务、调度和同步等更高层语义。

### 7.3 中断与设备抽象没有闭合

一边是内核自己的 ACPI/DT 探测路径，另一边是尚未完成的 `exarch::DeviceIf`；一边有
本地 timer，另一边通用 IRQ/IOAPIC/PLIC 尚未接通。这种“双轨”状态会增加后续驱动
选择接口的困难。ArceOS 虽然更复杂，但 IRQ、HAL、driver 和 runtime 已经形成相对
完整的依赖方向。

### 7.4 调试构建暴露出 RISC-V 高半指针问题

RISC-V 64 debug 启动在 `ecraos/src/mem.rs` 的高半地址指针 `.byte_add` 处触发 unsafe
前置条件检查，报告指针加法溢出。相同路径在 release 构建可以继续启动并完成定时器
测试，但这不能证明操作本身符合 Rust 的 pointer provenance 和算术约束。

这应被视为真实的安全性和构建一致性问题：不能仅以 release 可运行作为关闭依据。
应改为先在地址整数或语义地址类型上计算，再在实际需要解引用的边界构造指针，并
补充 debug/release 都执行的 RISC-V 启动验证。

### 7.5 系统级测试没有可靠判定成功或失败

`test.sh` 能构建并启动两个目标，是有价值的 smoke test；但脚本在 QEMU 前使用
`set +e`，且没有对关键日志和 QEMU 状态进行断言。本次 RISC-V debug 已经 panic，
脚本最终仍返回 0。这会让 CI 把启动失败误判成成功。

另外，裸机 loader crate 与普通 host tests 混在一个 workspace 中，直接运行
`cargo test --workspace` 会遇到 panic handler/main 冲突；当前全 workspace 的
x86-64 clippy 命令也会尝试处理不匹配目标的 loader 并失败。ArceOS 的配置矩阵同样
复杂，但其应用测试和平台 CI 覆盖明显更广。

## 8. ArceOS 相对优势及其代价

### 8.1 相对优势

ArceOS 当前最明显的优势不是某一个底层算法，而是已经闭合的运行时链条：

`应用 -> axstd/axlibc -> ArceOS/POSIX API -> axruntime -> task/fs/net/driver -> HAL`

这条链路带来以下能力：

- 应用主函数和退出流程。
- 多任务、抢占和多种调度策略。
- 设备发现、驱动注册和 IRQ 协作。
- 文件系统、页缓存、网络协议栈和 socket 接口。
- Rust 和 C 应用接口。
- 多架构、多平台和大量应用级测试。

它还为不同规模系统提供 feature 化裁剪，使同一套组件可以从较小的 unikernel 配置
扩展到更完整的运行环境。

### 8.2 相对代价

ArceOS 的成熟度伴随明显复杂度：

- crate、feature、宏和平台选择较多，理解一个具体构建需要同时确认多个配置层。
- 某些接口是否真实可用取决于 feature 组合，源码存在不等于当前配置已接入。
- 驱动覆盖广，但不能推断所有驱动在每个平台都经过同等程度的端到端验证。
- POSIX API 名称容易让读者误以为核心 ArceOS 已经提供完整 Linux 进程兼容，需要与
  StarryOS 明确区分。
- 更大的集成面也意味着配置兼容、初始化顺序和跨模块回归风险更高。

因此，ecraOS 不需要机械复制 ArceOS 的全部 crate 和 feature 结构。更合理的做法是
保留当前清晰的地址类型、loader 边界和内存层次，在引入下一层能力时确保只有一条
正式的运行路径，并为它建立可判定的测试。

## 9. 本次验证结果

### 9.1 实际运行

| 命令/配置 | 结果 |
| --- | --- |
| `TIMEOUT_SEC=25 ./test.sh` | x86-64 debug 单 CPU 启动、定时器和关机通过 |
| `QEMU_EXTRA_ARGS='-smp 4' TIMEOUT_SEC=25 ./test.sh` | x86-64 debug 四 CPU、AP 启动和 remote-free 通过 |
| `TARGET=riscv64gc-unknown-none-elf TIMEOUT_SEC=25 ./test.sh` | RISC-V debug 在 `mem.rs` 高半指针运算处 panic，但脚本错误地返回 0 |
| `PROFILE=release TARGET=riscv64gc-unknown-none-elf TIMEOUT_SEC=25 ./test.sh` | RISC-V release 单 hart、100 次 timer event 和关机通过 |
| `PROFILE=release TARGET=riscv64gc-unknown-none-elf QEMU_EXTRA_ARGS='-smp 2' TIMEOUT_SEC=15 ./test.sh` | RISC-V release 双 hart、remote-free 和 timer 通过 |

### 9.2 库测试和静态检查

以下选定 host-side crate 的测试通过：

```text
exbuddy, exslab, expt, expercpu, memory_range_set, maybe_non_generic,
size_disp, dyn_static_traits, expalloc_trait, ecraldr_base,
ecraldr_base_macros
```

合计通过 104 个单元/集成测试和 29 个 doctest。`cargo fmt --all -- --check` 通过。

以下命令不能作为当前仓库的有效绿色检查：

- `cargo test --workspace`：裸机 loader 作为 host test 构建时发生 duplicate
  `panic_impl` 和缺少 `main`。
- `cargo clippy --workspace --target x86_64-unknown-none`：全 workspace 会包含与该目标
  不匹配的 RISC-V loader，最终因 panic handler 等目标配置问题失败，同时还有若干
  warning。

本次只对 ecraOS 进行了上述实际构建和 QEMU 验证。ArceOS 结论来自当前源码、feature
连接和测试目录审阅，没有在本次会话中重新执行 ArceOS 的完整 QEMU 测试矩阵。

## 10. 建议的后续路线

### P0：先让现有路径可被可靠验证

1. 修复 RISC-V 高半地址 `.byte_add` 的 unsafe 前置条件问题。
2. 让 `test.sh` 保留并检查 QEMU 退出状态，对 panic 和关键启动日志作断言。
3. 将 host crate tests、x86-64 kernel/loader、RISC-V kernel/loader 的 check/clippy 命令
   分开，形成真正可持续的 CI 矩阵。
4. 为 AP/hart 启动增加超时和可诊断错误。

### P1：补齐中断与 SMP 协作闭环

1. 完成 x86-64 IOAPIC/IRQ 注册和分发。
2. 完成 RISC-V PLIC external interrupt 和 software IPI。
3. 建立通用 IPI API，并在其上实现 TLB shootdown。
4. 统一内核实际探测路径与 `exarch` 设备接口，避免长期保留两套抽象。

### P2：建立最小任务运行时

1. 定义 task、kernel stack 和上下文切换。
2. 先实现非抢占 FIFO scheduler、yield、block/wake 和 wait queue。
3. 将 timer 接入 sleep/timeout，再扩展为抢占调度。
4. 增加一个正式的内核应用入口，把当前自检迁移为测试应用而非内核主流程。

### P3：选择一个纵向应用场景

在任务和中断稳定后，优先选择一个能验证全链路的场景，而不是同时铺开所有功能。
例如：

- `virtio-blk -> block API -> 简单只读文件系统 -> 应用读取文件`，或
- `virtio-net -> smoltcp -> UDP echo -> 应用 socket API`。

这样的纵向切片能够同时验证 IRQ、DMA/内存映射、任务阻塞唤醒、资源生命周期和应用
接口，也更容易判断哪些 ArceOS 抽象值得借鉴，哪些会给 ecraOS 带来不必要的复杂度。

## 11. 主要源码依据

### ecraOS

- [`ecraos/src/main.rs`](../../ecraos/src/main.rs)：BSP/AP 入口、高半切换和当前自检流程。
- [`ecraos/src/mem.rs`](../../ecraos/src/mem.rs)：内存初始化和 RISC-V debug 触发点。
- [`ecraos/src/mem/vmm.rs`](../../ecraos/src/mem/vmm.rs)：VMM 模式和高半布局。
- [`ecraos/src/mem/allocs`](../../ecraos/src/mem/allocs)：slab、remote-free 和 `vmalloc`。
- [`ecraos/src/mp.rs`](../../ecraos/src/mp.rs)：CPU 枚举、栈和 AP/hart 启动。
- [`ecraos/src/device.rs`](../../ecraos/src/device.rs)：ACPI、DT 和设备探测。
- [`ecraos/src/timer.rs`](../../ecraos/src/timer.rs)：定时器初始化和自检。
- [`exarch/src/arch`](../../exarch/src/arch)：x86-64、RISC-V 64 平台实现和未完成接口。
- [`expt/src`](../../expt/src)：页表和地址类型。
- [`ecraldr`](../../ecraldr)：加载器和内核入口胶水。
- [`test.sh`](../../test.sh)：当前 QEMU smoke test。

### ArceOS/TGOSKits

- [`axruntime/src/lib.rs`](../../refs/tgoskits/os/arceos/modules/axruntime/src/lib.rs)：运行时统一
  初始化和应用入口。
- [`axtask/src/lib.rs`](../../refs/tgoskits/os/arceos/modules/axtask/src/lib.rs)：任务与调度。
- [`axstd/Cargo.toml`](../../refs/tgoskits/os/arceos/ulib/axstd/Cargo.toml)：应用层 feature
  组合。
- [`arceos_api/src/lib.rs`](../../refs/tgoskits/os/arceos/api/arceos_api/src/lib.rs)：Rust API。
- [`arceos_posix_api/src/lib.rs`](../../refs/tgoskits/os/arceos/api/arceos_posix_api/src/lib.rs)：
  POSIX 风格 API。
- [`axfs-ng`](../../refs/tgoskits/os/arceos/modules/axfs-ng)：文件系统初始化和后端。
- [`ax-net/src/lib.rs`](../../refs/tgoskits/net/ax-net/src/lib.rs)：网络栈结构和初始化。
- [`axcpu/src/lib.rs`](../../refs/tgoskits/components/axcpu/src/lib.rs)：多架构 CPU 支持。
- [`someboot/src/lib.rs`](../../refs/tgoskits/platforms/someboot/src/lib.rs)：多架构启动支持。
- [`TGOSKits README`](../../refs/tgoskits/README.md)：ArceOS、StarryOS 和 AxVisor 的项目边界。
- [`StarryOS kernel`](../../refs/tgoskits/os/StarryOS/kernel/src)：Linux 用户任务、ELF 和系统
  调用层，用于说明其不属于 ArceOS 核心。

## 12. 总结

ecraOS 当前最值得保留的不是“比 ArceOS 少”，而是已经建立的几项清楚原则：加载器
与 PIE 内核分离、语义地址类型、可追踪的 VMM、分层 allocator，以及通过真实多核
启动验证 remote-free。它们构成了继续向上建设的良好基础。

当前最重要的问题也不是立即追平 ArceOS 的功能数量，而是先关闭已有基础设施中的
正确性和验证缺口，再按“中断与 IPI -> 最小任务运行时 -> 单一纵向设备/应用场景”
的顺序向上扩展。这样既能借鉴 ArceOS 已经证明有效的模块边界，又能避免过早复制其
feature graph 和集成复杂度。
