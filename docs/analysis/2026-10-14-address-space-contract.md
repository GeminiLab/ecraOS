# ecraOS AddressSpace 设计讨论起点

Created: 2026-10-14

Updated: 2026-10-15

Status: 讨论暂停时的交接草稿，尚未形成完整设计。

## 阅读与恢复指南

本文应能在没有原始对话记录的情况下，作为下一次 AddressSpace 设计讨论的起点。
恢复讨论时，先阅读本节，再按第 7 节的使用场景推进。源码定位与核对基线见第 8 节，
PageTable 的既定生命周期约束见第 3 节。

**讨论停在这里：AddressSpace 表示逻辑地址转换域，当前只研究 host VA → PA，
ecraOS 需要提供其加载和切换基础设施。尚未决定 AddressSpace 的具体形态及加载接口。**

| 状态 | 内容 | 位置 |
| --- | --- | --- |
| 已确认 | 从系统软件的使用方式推导抽象，支持以 Unikernel 为基座的可组合系统 | 第 1、4 节 |
| 已确认 | AddressSpace 与 PageTable 分层，前者处理映射策略，后者管理具体 hierarchy | 第 2、3 节 |
| 已确认 | ecraOS 管理 host VA → PA，二阶段翻译由 hypervisor 独立管理和加载 | 第 1 节 |
| 已确认 | 内核高半区与用户低半区的区域管理需求不同 | 第 4.2 节 |
| 已确认 | 共享区域或映射策略需要明确表达，可以使用独立 table frames 实现 | 第 5.1 节 |
| 未决定 | 一个 AddressSpace 与 PageTable 实例、roots、硬件加载配置的数量及组合关系 | 第 6 节 |
| 未决定 | KPTI 使用一个多形态 AddressSpace，还是上层配对的两个 AddressSpace | 第 6.1 节 |
| 未决定 | AArch64 TTBR1 复用、其他架构的半区复用，以及共享更新协议 | 第 5.2、6.2 节 |
| 未决定 | 区域类型、公开操作、Task 关联、并发和销毁协议 | 第 7 节 |

后续应延续以下工作方式：一次讨论一个角度，先明确使用方式和契约，再考虑类型与代码。
本次交接没有授权实现 AddressSpace。最初的 expt 全面审查工作清单也不构成恢复讨论后
自动执行大范围修复的要求。

本文中，“已确认”是设计约束，“候选”是待比较的方向，“源码现状”是特定时间的观察。
三者不能相互替代。尤其不能因为原则已确认，就认为其实现及跨架构正确性已经验证。

## 1. 文档范围

讨论最初来自 expt 的契约审查。在明确 PageTable 的 table-frame 管理责任之后，
需要进一步回答它应提供哪些操作。但“区域是否占用、是否允许覆盖映射”等判断属于
更高层的 AddressSpace，因此先转向 AddressSpace 的使用模型，再反推 expt 的能力。

当前讨论聚焦 **host VA → host PA（HVA → HPA）**。ecraOS 应提供这一翻译域的维护、加载
和切换基础设施。二阶段翻译 **guest PA → host PA（GPA → HPA）** 可以在未来复用
AddressSpace 的基础设施，但由 hypervisor 自行管理和加载。两者可以共存，当前设计不把
它们合并到一个 AddressSpace，也不要求 ecraOS 调度器管理二阶段翻译。

设计从使用方式出发，先厘清 Unikernel、宏内核、微内核及 hypervisor 对 host 地址空间
的需求，再推导抽象和实现。Linux 等已有系统提供参考，ecraOS 的目标则是类似 ArceOS
的可组合基座：上层可以构建不同形态的系统软件，基础设施不能预设只有一种进程模型。

## 2. 术语与抽象边界

| 术语 | 含义 |
| --- | --- |
| AddressSpace / AddrSpace | 一个逻辑地址转换域，本文限于 host VA → PA |
| `multi-level page table (structure)` | 表示完整多级页表结构的抽象 Rust 数据类型 |
| `page table hierarchy` | 硬件使用的页表层级，以及存在于内存中的具体层级数据结构 |
| `page table` | 层级中某一级上的单张页表 |
| `table frame(s)` / `page table frame(s)` | 存储单张或多张 page table 的物理页面 |
| data frame | 叶子映射所指向的物理页面，不属于 expt 的页面管理范围 |
| backing | 区域内容的来源及其资源关系，不预设具体 Rust 类型 |
| 硬件加载配置 | 安装 host 地址转换所需的架构状态，不预设它与 AddressSpace 的数量关系 |

AddressSpace 与 PageTable 属于不同抽象层级。AddressSpace 处理区间用途、映射策略、共享
关系和操作是否合法。PageTable 管理具体 hierarchy 及其 table frames，执行底层 PTE
修改，并提供准确的失败行为与本地失效契约。

例如，“这个区间已经被占用，不允许建立新映射”属于 AddressSpace 的策略判断。
PageTable 的底层操作可以具有覆盖 descriptor 的语义，但必须明确覆盖产生的结构变化、
table-frame 生命周期和失效要求。两层的错误检查不能相互替代。

AddressSpace 不等同于一个 root 地址。“逻辑地址转换域”的定义也未决定它是否必须覆盖
全部 canonical 地址范围，或者如何与内核公共部分组合。第 6 节保留这些表示问题。
“硬件加载配置”只是讨论用语，尚不是决定新增的 Rust 类型。

## 3. 已确认的 PageTable 生命周期与运行前提

### 3.1 构造、页面管理与销毁

expt 负责 hierarchy 使用的全部 table frames，包括 root、child tables 和大页拆分
分配的 tables。叶子映射的 data frames 及硬件使用状态由外部管理。

- `PageTable<M, PTE, A>` 通过类型参数 `A: PageAllocator` 获得页面管理能力。
  不需要保存运行时 allocator 实例，也不引入动态 allocator 或另一种借用型 PageTable。
- `new_alloc` 分配并清零 root page table。后续操作按需分配 child tables。
- `new_at` 从既有 hierarchy 构造具有完整页面管理责任的值，不是借用型访问。
  调用者必须保证 hierarchy 是合法树，且选定 allocator 能访问和 deallocate 既有 frames。
- `Drop` 必须 deallocate 该多级页表结构管理的全部 table frames，不 deallocate data frames。
- `leak` 消耗该值并保留 table frames，返回 root。随后可以使用 `new_at` 重建，必要时
  更换 allocator 类型，前提是新 allocator 能管理全部既有 table frames。
- `root` 只返回地址，不转移页面管理责任，也不延长 table frames 的生命周期。

同一 hierarchy 不能同时由多个 PageTable 值独立管理。`OpaquePageTable` 擦除具体格式和
allocator 类型，但遵守相同生命周期契约。其内部临时重建具体值完成操作后，应保留
hierarchy，不能触发 table-frame deallocation。exarch 选择具体格式时，可以接受调用方
提供的 allocator 泛型参数，不需要直接依赖内核的 early 或正式 allocator 实现。

early → 正式 allocator 的迁移通过 `leak` 和重新构造表达，物理 hierarchy 本身可以保持
不变。接管之前，正式 allocator 必须已建立管理这些既有 table frames 所需的状态。

PTE 清零、descriptor 覆盖、child table 从 hierarchy 断开和 table-frame deallocation
是不同动作。descriptor 覆盖不能免除被断开 tables 的管理责任。覆盖导致下层页表
泄漏是已知实现缺陷，本文按修复后的预期契约描述。第 8 节记录源码基线及其限制。

### 3.2 软件访问与硬件使用

软件侧需要保证通过 PageTable 进行访问，并排除绕过它的冲突访问。cursor 的 `&mut`
借用只约束通过该 Rust 值进行的访问，不能阻止硬件 page walks 或其他 CPU 的 TLB 使用。

允许 CPU 使用活动 hierarchy 时执行 map/unmap。调用者负责提供必要的并发协调和
远程 TLB 失效。允许在线修改不表示硬件可以观察任意修改中间态，PTE 发布和替换的
架构顺序仍需由后续 mutation 契约明确。

在 `Drop` 并 deallocate table frames 之前，调用者必须让所有 CPU 停止使用该 hierarchy，
并完成所需的失效与同步。这个前提强于“cursor 已完成本地 flush”。

`TlbFlushes::flush_locally` 仅表示调用 CPU 的本地失效。cursor 的显式 `flush` 和 Drop
处理 pending cleanup 中的本地失效，不完成远程 acknowledgement，也不证明旧 table/data
frames 已经可以安全 deallocate。调用者可以通过 `take_cleanup` 取得 cleanup，完成所需
的远程同步后再处理 deallocation。expt 不建立第二套 CPU inventory、IPI 或远程等待机制。

这一运行前提的澄清是文档工作。它本身不要求更改 PageTable API，也不能据此声称
当前 kernel 已完整实现远程失效协议。

## 4. AddressSpace 的使用需求

### 4.1 访问环境与翻译阶段

从访问者看，AddressSpace 基础设施应能容纳以下用途：

| 访问用途 | 例子 |
| --- | --- |
| 仅供管理它的特权级使用 | 典型 Unikernel 的内核地址空间 |
| 仅供更低特权级使用 | 将来由 hypervisor 管理、供 guest 使用的 GPA → HPA 域 |
| 供管理特权级及更低特权级共同使用 | 宏内核或微内核中的 host 地址空间 |

“当前特权级”在这里相对于管理该域的环境而言，不是固定的架构级别，也不是随调用点
变化的权限定义。这一用途分类不直接等同于某个 PTE 权限位。通过另一地址空间访问
某个 data frame，也不意味着使用了该 frame 所属映射的翻译域。

访问环境与翻译阶段是两个维度。第二项说明基础抽象将来的可复用需求，不把 GPA → HPA
重新纳入当前 host VA → PA 的设计范围。具体访问权限及可见性如何进入 API，尚未确定。

### 4.2 内核与用户区域的管理差异

在当前目标布局中，高半区用于内核，低半区用于用户执行环境。高、低地址和特权级用途
之间的对应属于布局策略，不应被当作所有 AddressSpace 或翻译阶段的固有定义。

内核高半区通常具有统一布局，例如内核映像、物理内存 direct map 和动态分配区域。
大部分映射由启动及全局布局决定，主要在 vmalloc 等范围内需要动态区域管理。
用户低半区则需要根据各个执行环境的需求管理不同区域及其映射。

因此，不能先要求所有 AddressSpace 都使用一份覆盖完整地址范围、采用相同策略的
动态区域集合。固定布局与动态区域如何组合，应从实际使用方式推导。

区间的预留状态与 PTE 是否存在是不同状态。例如 guard 或按需填充的范围可能已被
预留，但没有叶子映射。这说明仅查询 PTE 无法替代区域状态，尚不足以确定区域模型。

曾提出“每个 AddressSpace 具有一个全局、互不重叠的区域集合”的方案，但尚未确定。
是否把每个 guard 记录为独立区域、区域最小字段及具体操作，也没有定案。应先确定
不同使用场景如何组织 AddressSpace，再决定哪些范围需要区域集合及其管理方式。

### 4.3 系统形态与加载动作

| 系统形态 | host AddressSpace 的使用需求 |
| --- | --- |
| Unikernel | 通常使用统一 host 地址空间，任务切换可以保持原有地址转换 |
| 宏内核 | 不同用户执行环境具有各自的区域状态，通常共享内核映射策略 |
| 微内核 | 内核、服务及应用的地址转换和共享关系由上层组织，不预设 Linux 进程模型 |
| hypervisor | ecraOS 维护 host HVA → HPA，hypervisor 另行维护和加载 guest 的二阶段翻译 |

ecraOS 应为 host AddressSpace 提供加载和切换基础设施。在宏内核或微内核场景中，
Task 切换在内核态进行，可能产生 host AddressSpace 切换。多个 Task 也可能使用同一
AddressSpace，因此任务切换与地址空间切换不是一一对应的动作。

加载地址转换不转移 table frames 的页面管理责任。CPU 停止使用之前，层级及其 frames
必须保持有效。加载动作怎样表达、Task 怎样关联 AddressSpace、如何记录各 CPU 的
使用状态和完成销毁同步，均尚未确定。

## 5. 共享关系

### 5.1 已确认的语义方向

共享 data frames 是普通 backing 关系：不同 AddressSpace 可以把同一物理内存映射到
不同地址，并具有不同权限。具体 data-frame 生命周期可以由 backing 或上层协调。
这一点本身不要求 AddressSpace 提供特殊的跨空间映射策略。

本次重点是 **共享区域或映射策略**。例如多个用户 AddressSpace 使用共同的内核高半区
布局，动态内核映射变化需要被相关 host 硬件配置体现。AddressSpace 必须能够理解这类
关系，不能让每个实例各自维护互不相关的内核映射副本。

已认可的基本方向是：共享内核映射具有共同的逻辑描述或更新来源，各个具体 hierarchy
可以使用独立 table frames。这里共享的是映射意图，尚未决定传播时机、同步保证及
更新失败后的处理方式。

### 5.2 table frames 与 roots 的复用

任意共享 page table frames 或子树涉及额外的生命周期和并发规则，目前暂不展开。
上述共享策略不要求多个 PageTable 值管理同一批 frames。

共享半区时复用某个完整 root 是已提出、尚未选定的优化候选。它不能悄悄破坏
第 3 节的生命周期契约。AArch64 复用独立 upper root 和单根架构共享中间子树存在
结构差异，不能仅以“复用半区”概括为同一个底层操作。

## 6. 候选表示与尚未解决的问题

以下内容用于保存备选方向和取舍理由，不代表选择了其中任何方案。
不能用“AddressSpace 是逻辑转换域”直接推导 AddressSpace、硬件加载配置、root 或
PageTable 实例之间的一对一或一对多关系。

### 6.1 KPTI

KPTI（Kernel Page Table Isolation）在低特权执行与内核执行时使用不同的内核映射
可见范围。对 host AddressSpace 的建模存在两种候选：

1. 一个逻辑 AddressSpace 具有多个硬件加载配置，用户态和内核态选择不同的映射内容。
2. 两个 AddressSpace 共享用户低地址空间的逻辑状态，由更高层对象把它们组织为一对。

| 比较项 | 一个 AddressSpace，多种加载配置 | 两个 AddressSpace，上层配对 |
| --- | --- | --- |
| 逻辑用户区域 | 可由同一对象集中管理 | 需要定义共享用户区域状态的方式 |
| 普通场景的成本 | 需避免让无 KPTI 场景承担多可见范围的复杂度 | 配对能力可放在可选上层 |
| 更新与失败 | 需要协调不同配置的更新及部分完成 | 需要协调一对地址空间的更新及部分完成 |
| 加载动作 | 上层选择同一对象的不同配置 | 上层选择配对中的不同对象 |

讨论中提出的关键约束是：不能为了 KPTI 让其他所有 AddressSpace 场景都承担
“两个可见范围”的负担。如果选择第一种，必须说明不同形态如何隔离这种复杂度。
这项约束已明确，但不足以据此宣布第二种胜出。

“AddressSpace 表示逻辑地址转换域”已经认可，不代表上述任一表示已经定案。
KPTI 也不能被简单等同于用户区域和内核区域分别放到两个完全无关的地址空间。
一般的 KPTI 内核配置仍可包含用户映射，用户配置保留必要的内核入口映射。

### 6.2 AArch64 TTBR0 / TTBR1 与半区共享

AArch64 使用 TTBR0 和 TTBR1 对应的 roots 组织低、高地址翻译。x86-64 和 RISC-V 的
当前模式则使用单 root。硬件 root 数量本身不决定逻辑 AddressSpace 数量。

在多用户 AddressSpace 的系统中，共同内核高半区如何对应 TTBR1 仍需明确。
候选包括独立 upper hierarchies 配合更新传播，以及复用统一 upper hierarchy/root。
这可能要求架构特化的组合或加载接口。

复用 TTBR1 root 不一定要求构造多个独立管理相同 frames 的 PageTable 值。
可以研究由一个对象管理完整 upper hierarchy，再在硬件加载时与不同 lower roots
组合的方式。这只是候选分析，不是已批准的 Rust 类型或生命周期设计。

对于 x86-64/RISC-V，复用内核半区可能需要在不同 roots 下引用相同子树，这与当前
合法树及独立 table-frame 管理的约束不同。不能因 AArch64 可以组合两个独立 roots，
就假设单根架构无需额外生命周期设计。

## 7. 下一次讨论的具体起点

### 7.1 从无 KPTI 的 Task 切换开始

建议恢复时首先讨论以下场景，不立即选 Rust 类型：

> 系统有两个用户执行环境 A、B，各自具有不同的用户低半区，并使用共同的内核高半区
> 映射策略。CPU 已因中断或系统调用从 A 进入内核，调度器在内核态切换到 B，
> 最后返回 B 的用户态。暂不启用 KPTI，也不直接共享 table frames。
> 在这个过程中，上层应该向 ecraOS 提交什么对象，ecraOS 应加载哪些地址转换状态？

这是建议的切入点，不是已决定“一进程一个完整 AddressSpace”的实现。
可用 A、B 表示执行环境而非具体进程类型，避免先绑定宏内核的进程模型。

围绕这条路径逐项明确：

| 时刻 | 需要回答的问题 |
| --- | --- |
| 建立 A、B | 用户私有区域和内核公共部分分别由什么对象表达 |
| A 进入内核 | 沿用原 host 地址转换，还是执行显式加载动作 |
| 内核从 A 切到 B | 调度器提交逻辑 AddressSpace，还是更高层的加载组合 |
| 安装 B 的转换 | 哪些内容必须保持可访问，包括切换代码、内核栈及 per-CPU 数据 |
| 返回 B | 是否还有独立于 Task 切换的加载步骤 |
| 切换不改变 AddressSpace | 两个 Task 使用同一转换域时，上层如何表达这一点 |

首先要达成的结果是一条无歧义的调用时序，以及各对象的责任边界。
不必在这一轮决定锁、CPU 使用记录、引用计数或汇编实现。

### 7.2 用其他使用场景检验

第 7.1 节的模型明确后，再依次检查：

1. **Unikernel**：多个 Task 使用同一个 host 地址空间，任务切换通常无需切换地址转换。
   验证基础模型没有引入不必要的进程或多配置负担。
2. **KPTI**：分别沿 Task 切换、进入内核、返回用户的路径比较第 6.1 节两种候选，
   找出共享用户状态和选择加载配置的责任所在。
3. **架构差异**：把相同使用路径投影到 x86-64/RISC-V 单 root 和 AArch64 双 root，
   明确内核公共部分如何参与组合。root 复用仍可单独延后。
4. **hypervisor 的 host 部分**：确认 host HVA → HPA 的管理不会要求 ecraOS 同时
   接管 GPA → HPA 的加载。二阶段接口及 vCPU 状态机继续在当前范围之外。

### 7.3 在使用模型之后讨论

完成上述比较后，再回到以下问题，避免过早把最初的区域集合建议变成基础类型：

- 固定内核布局、vmalloc 动态范围和用户区域如何组合，区域至少需要表达什么。
- 共享映射策略由谁维护，何时传播，更新失败及部分完成时提供什么保证。
- AddressSpace 应提供哪些操作，哪些操作由 backing、vmalloc 或更高层完成。
- 加载/切换如何与多 CPU 使用状态、远程失效及销毁同步配合。
- 从这些需求反推 expt 的查询、遍历、覆盖、属性修改、大页处理和失败契约。

当前尚未选择具体 trait、字段、方法名、锁或引用计数方案。对不存在范围执行 unmap
是否分配 tables，也尚未定案。不能仅因已有行为不理想，就跳过分层讨论直接修改。

ASID/PCID、CPU hotplug、二阶段页表及任意子树共享不作为本次恢复讨论的前置实现任务。

### 7.4 后续更新本文的方式

每次只将明确达成共识的内容提升为“已确认”，并说明解决了哪个问题及其理由。
候选方案保留各自适用前提，确认取舍后再简要记录未采用方案的原因。
如果源码变化影响基线描述，更新第 8 节，不把旧观察当作当前事实。
这样本文可以逐步形成技术设计，而不需要依赖对话时间线。

## 8. 源码基线、核对方法与参考

### 8.1 交接时的实现状态

2026-10-15 整理时核对的 HEAD 为：

```text
cb6617ba02a74b8f3c0a78fcc6b249ea16afdb86
refactor(expt): make PageAllocator a type param of [Dual]PageTable, let page tables manage their frames
```

该基线中，PageTable 已通过类型参数选择 allocator，并具有 `new_alloc`、`new_at`、
`leak` 和 Drop 路径。内核使用一个全局 `VirtualAddressSpace` 及 `OpaquePageTable`，
vmalloc 单独维护动态虚拟区间。当前类型名不意味着已经实现本文讨论的通用多实例
AddressSpace 模型。

expt 的 crate 文档明确记录了映射覆盖断开 child tables 后丢失其地址的已知泄漏，
当前 Drop 不能因此被视为已完整满足第 3 节的预期 deallocation 契约。
软件访问与硬件使用边界的文档表述也需要与第 3.2 节对齐，不能默认已全部落地。

本次交接只整理文档和核对相关源码，没有执行编译、host tests 或 QEMU。
本文不继承此前 session 的测试结果作为未来基线，也不宣称远程失效或架构 mutation
顺序已经完成正确性证明。

### 8.2 恢复时重新核对

实际恢复讨论的时间不确定，因此先读取当时的 AGENTS.md，检查工作区并保留既有修改。
可用下列命令确定源码身份和与本文基线的差异：

```sh
git status --short --branch
git rev-parse HEAD
git log --oneline -8
git diff cb6617ba02a74b8f3c0a78fcc6b249ea16afdb86 -- expt exarch ecraos/src/mem
```

如果历史已重写或基线不存在，应依据下列模块及 API 重新定位，不要求恢复旧分支。
阅读源码差异是为了修正事实背景，不自动触发实现或完整回归测试。

### 8.3 源码阅读入口

- [`mem/vmm.rs`](../../ecraos/src/mem/vmm.rs)：`VirtualAddressSpace`、内核布局、
  `with_page_table` 和 allocator handoff，观察全局映射管理现状。
- [`mem/allocs/vmalloc.rs`](../../ecraos/src/mem/allocs/vmalloc.rs)：动态虚拟区间和
  backing 状态，观察现有区域管理职责。
- [`expt/src/lib.rs`](../../expt/src/lib.rs)：crate 契约、术语及已知实现限制。
- [`expt/src/pt/single.rs`](../../expt/src/pt/single.rs)：静态 allocator、构造、leak、
  Drop 和 cursor 操作。
- [`expt/src/pt/dual.rs`](../../expt/src/pt/dual.rs)：lower/upper roots 的具体组合。
- [`expt/src/opaque.rs`](../../expt/src/opaque.rs)：格式和 allocator 能力的类型擦除。
- [`exarch/src/parts/mem.rs`](../../exarch/src/parts/mem.rs)：模式选择、
  `get_page_table_type` 和 `set_page_table_root` 接口。
- [`x86_64/mem.rs`](../../exarch/src/arch/x86_64/mem.rs)、
  [`riscv64/mem.rs`](../../exarch/src/arch/riscv64/mem.rs)、
  [`aarch64/mem.rs`](../../exarch/src/arch/aarch64/mem.rs)：各架构的 root 安装方式。

这些链接用于定位职责和符号，后续路径变化时按符号搜索即可。

### 8.4 外部参考

Linux 参考以 v6.12 为观察对象，用于理解职责分层，不作为已经完成的跨架构正确性证明：

- [Page Tables](https://docs.kernel.org/6.12/mm/page_tables.html)：层级、地址空间及映射背景。
- [`include/linux/pagewalk.h`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/include/linux/pagewalk.h)：遍历与 hole 回调。
- [`include/asm-generic/tlb.h`](https://kernel.googlesource.com/pub/scm/linux/kernel/git/torvalds/linux/+/refs/tags/v6.12/include/asm-generic/tlb.h)：失效记录与延迟内存回收的边界。

对 Linux/ArceOS 的具体 API、KPTI 实现或架构规则作进一步比较时，需固定源码或规范版本。
这些参考不会替代对 ecraOS 使用方式的讨论，也不意味着采用与其相同的类型组织。
