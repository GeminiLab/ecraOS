# GitHub CI 超时背后的同步竞态：发现、分析与修复

日期：2026-11-11。

本次排查由维护者报告的 GitHub CI 间歇性 `TIMEOUT` 发起。具体竞态的定位、
确定性复现、修复代码、回归测试和验证由 AI 编码助手 Codex（gpt-6.1-sol）完成，
并由另一个 AI 审查实例独立检查并发协议。审查中还发现了一个影响许可判断的
过期定时器唤醒边界，随后补充了回归和防护。本文也由该 AI 助手编写。

排查以修复前的 `3f555f7f` 为基线。下文的源码链接指向与本文同一变更集中的修复实现。

## 1. 发现背景与 CI 证据

维护者发现，GitHub CI 中的以下命令在多次运行时较容易超时：

```bash
cargo xtask qemu --crate smoke-apps/sync-preemption \
    --target riscv64gc-unknown-none-elf \
    --profile debug,release --cpus 1,4 --timeout 20
```

[CI 工作流](../../.github/workflows/ci.yml)以架构和 smoke 应用划分 job，
上述命令再展开 debug/release 与 1/4 CPU 的四种组合。另一个 `rustsbi-tests` job
使用 RustSBI 固件运行 RISC-V smoke，单次 guest 超时为 30 秒。

排查下载并检查了两份历史失败日志：

| 日期 | CI 运行 | 失败组合 | 最后进度与结果 |
| --- | --- | --- | --- |
| 2026-11-04 | [RustSBI job](https://github.com/GeminiLab/ecraOS/actions/runs/41794832122/job/126456358071) | RISC-V，debug，4 CPU | `Semaphore releaser started` 后无后续进度，30 秒超时 |
| 2026-11-06 | [x86-64 job](https://github.com/GeminiLab/ecraOS/actions/runs/42207544794/job/127857854628) | x86-64，release，1 CPU | 同一位置停止，20 秒超时 |

两份日志在停止前都已输出 `Stage 2B Condvar join passed`，随后输出信号量任务
已创建、获取者启动和释放者启动，却没有输出获取者获得许可或整个 smoke 成功的标记。
这将优先排查范围缩小到了信号量阶段。x86-64 单核也发生同类停顿，说明不能只按
RISC-V 多核特有问题处理。

本地 QEMU 与 CI 同为 9.2.3。修复前原始命令连续复跑 30 轮，合计 120 个组合
全部通过，正常 guest 通常在 1 秒以内完成。这说明普通重复测试没有覆盖到失败交错，
不能据此排除实现错误，也不足以支持直接提高超时上限。

CI 日志没有完整的任务状态快照，因此不能仅凭最后一行证明每一次历史超时都具有
完全相同的原因。以下结论来自源码分析和强制调度交错的复现，其中第一项与上述
CI 停顿位置一致，另外两项是在同一同步模块中扩展检查时发现的独立错误。

## 2. Semaphore：检查许可后丢失唤醒

旧实现的 `Semaphore::acquire()` 先读取许可数，发现为零后，再调用调度器将
当前任务登记到等待队列。这两个步骤没有与 `release()` 共享一个完整的临界区。

可能的执行顺序如下，其中 A 为获取者，B 为释放者：

```text
A: 读取 permits == 0，决定阻塞，尚未登记等待者
A: 被抢占
B: release() 发现等待队列为空，将 permits 从 0 增加到 1，返回
A: 恢复执行，仍按旧判断登记等待并阻塞
结果: permits == 1，但 A 已阻塞，且没有后续释放者负责唤醒它
```

主任务随后等待 A 的 `join()`，整个 guest 不再结束。这种交错在单核也成立，
只需在资源检查与登记之间发生一次抢占，不要求两个 CPU 同时执行。

AI 在这两个步骤之间临时插入一次 `yield_now()`，使 B 确定先执行。
RISC-V 单核的 debug 和 release 均稳定超时，阻塞前的诊断值为：

```text
sampled=0, available=1, waiters=0
```

即先前采样为零，但实际已经有一个可用许可，A 仍然进入等待。原子计数本身没有
丢失更新，错误在于“检查状态并决定等待”与“发布等待者”不是一个不可分割的协议。
提高原子内存序不能关闭这个窗口。

修复后的许可消费、等待登记、释放选择都持有同一把信号量等待队列锁。
`prepare_acquire()` 在锁内重新检查许可与当前获取记录：如果已经可以获取资源，
便不登记、不改变任务状态，返回获取循环重试。否则在仍持锁时完成
`Running -> Blocked` 和队列登记。释放路径无法插入检查与登记之间。

## 3. Mutex：加锁失败后丢失解锁通知

旧实现的 `Mutex::lock()` 有同类窗口：`try_lock()` 失败后，才调用调度器登记等待者。

```text
A: try_lock() 失败，尚未登记等待者
B: 持有者解锁，返回受保护值，发现等待队列为空，因此不唤醒任何任务
A: 登记等待并阻塞
结果: mutex 已空闲，但 A 仍永久阻塞
```

AI 同样通过一次明确的调度切换强制解锁发生在窗口中。RISC-V 单核的 debug/release
都稳定超时，A 阻塞前的诊断值为：

```text
unlocked=true, waiters=0
```

修复后的 `try_lock()`、等待前的准入检查和 `unlock()` 都使用该 Mutex 的
等待队列锁。`WaitQueue::prepare_block_if()` 在持锁时重新执行准入判断，
只有确实仍需等待才登记任务。

`unlock()` 在同一锁内返回受保护值、清除当前 owner、选取 FIFO 等待者并为其
设置 `reserved`，释放锁之后才发布唤醒。Mutex 仍保留单个预留 owner，因为它
每次只交接一个所有权，下一次合法解锁必须发生在该接收者实际获取并释放之后。

这也封闭了另一条相关窗口：新调用者不能在检查预留 owner 之后、实际取走值之前，
绕过刚刚建立的 FIFO 所有权预留。

## 4. Semaphore：全局 reserved 槽覆盖先前许可

旧 Semaphore 只有一个 `reserved: AtomicUsize`，记录获得下一份许可的任务 ID。
与 Mutex 不同，信号量允许在任何等待者恢复之前连续执行多次释放。

对于两个已经阻塞的等待者 A、B，会发生：

```text
release #1: 移除 A，写 reserved = A，唤醒 A
release #2: 移除 B，写 reserved = B，唤醒 B
A: 恢复，发现 reserved 不再是自己，也没有可用许可，再次阻塞
B: 恢复，领取 reserved 中的许可，成功返回
```

第一份许可随 `reserved` 的覆盖而失去归属。AI 使用 `Semaphore::new(0, 2)`、
两个等待者和连续两次 `release()` 构造复现，不需要在原语内部插入调度。
debug/release 都只完成第二个等待者，诊断结果为：

```text
first=false, second=true, available=0
```

原 CI smoke 只有一个信号量获取者，因此这一项不能单独解释它的超时，
但它是多等待者场景中的实际错误。

最终实现移除了 Semaphore 的全局 `reserved`，改为每次等待获取拥有独立的
`Arc<SemaphoreWaiter>`。记录中保存任务引用和 `AtomicBool granted`：

- 获取者与等待队列共同持有同一记录。
- `release()` 在锁内弹出 FIFO 队首，设置该记录的 `granted = true`，
  然后释放锁，再通知调度器唤醒对应任务。
- 被移交的许可不增加公共 `permits`，`available()` 不把已分配给等待者的许可
  计为可用许可。没有等待者时，释放才增加公共计数并检查上限。
- 获取者只读取自己的授权记录。其他获取记录的授权和后续释放不会覆盖它。

`granted` 通过 Release 存储和 Acquire 读取发布，资源计数及等待队列的联合操作
由队列锁串行化。独立授权记录的生命周期覆盖锁释放与调度唤醒之间的间隔。

## 5. AI 独立审查发现的边界：唤醒不等于许可

第一版修复尝试把“已经登记的 Semaphore 等待返回”直接视为获得许可。
AI 独立审查指出，这依赖于不存在其他唤醒来源，而当前定时器路径并不提供该保证。

审查给出的交错是：某个 CPU 已将过期定时器记录提取到本地待处理集合，
另一 CPU 通过通知结束了任务的 `Condvar::wait_timeout()`。取消操作无法移除
已经提取的记录。任务随后可能在同一代次下进入无超时的 Semaphore 等待。
旧定时器回调稍后执行时，`wake_from_sleep()` 的后备分支可将 `Blocked` 状态
改为可运行，而这个唤醒并没有释放信号量许可。

AI 为此新增了确定性回归：任务完成一次真实 sleep，随后阻塞在零许可信号量上，
通过测试接口重放旧定时器唤醒路径。第一版修复确实触发了以下断言：

```text
semaphore returned without a permit after a stale timer wake
```

最终实现因此要求显式检查该次获取的 `granted`，不把等待返回本身作为授权。
任务被无关唤醒后，如果还没有许可，就保留原有等待记录和 FIFO 位置，
重新提交阻塞状态，不重复登记队列。只有实际的 `release()` 能设置授权标志。

本次解决的是这种无关唤醒对 Semaphore 许可语义的影响。
`wake_sleepers()` 和 `wake_from_sleep()` 的定时器状态／代次校验实现没有在
本次修改中重写，不能把上述回归通过解释为定时器竞争本身已被完整修复。

## 6. 对其他 OS 的参考与 ecraOS 的调度边界

本次修复参考了 [Linux v6.12 的 counting semaphore 实现](https://github.com/torvalds/linux/blob/v6.12/kernel/locking/semaphore.c)：
`sem->lock` 保护计数与等待队列，每个 `semaphore_waiter` 各有一个 `up` 标志。
释放方选中一个等待者后先设置它的标志，再唤醒任务，等待方恢复后检查自己的标志。
这里参考的是共享锁保护登记和逐等待者授权的协议，ecraOS 的实现仍使用自身的
Rust 所有权、IRQ 安全锁、任务状态机和调度器接口。

源码入口：

- [同步原语与等待记录](../../ecraos/src/task/sync.rs)：
  `WaitQueue::prepare_block_if()`、`Mutex`、`Semaphore`、`SemaphoreWaiter`。
- [调度器](../../ecraos/src/task/scheduler.rs)：`block_current_if()` 和唤醒发布。
- [任务生命周期](../../ecraos/src/task/types.rs)：阻塞、唤醒与当前栈交接。

`block_current_if()` 禁止在抢占保护临界区内提交阻塞，在本地 IRQ 关闭时调用
原语提供的登记操作。该操作必须在返回前释放资源锁。条件已改变时返回 false，
不修改任务运行状态。确需等待时提交阻塞，再由调度器选择下一个任务。

资源锁、等待队列锁不跨越上下文切换，也不跨越远程调度通知。释放方可以在
任务实际切出之前完成唤醒，任务生命周期与现有切换交接协议负责延后可运行发布，
避免另一个 CPU 恢复尚未保存的上下文。

## 7. 回归测试与验证结果

[同步 smoke 回归](../../smoke-apps/sync-preemption/src/regressions.rs)在 BSP 的
cooperative domain 中执行，完成后再进入原来的 preemptive smoke：

| 回归函数 | 强制覆盖的条件 | 修复前结果 |
| --- | --- | --- |
| `semaphore_release_before_registration` | 观察零许可后、登记前释放 | 丢失唤醒 |
| `mutex_unlock_before_registration` | 加锁失败后、登记前解锁 | 丢失唤醒 |
| `semaphore_releases_preserve_each_waiter` | 两次释放先于两个等待者恢复 | 第一份授权被覆盖 |
| `stale_timer_cannot_grant_a_permit` | 不释放许可，只重放旧定时器唤醒 | 第一版修复错误返回成功 |

前三项分别在旧实现上运行并确认命中对应断言，再进行修复。第四项在 AI 审查后
加入，先证实隐式授权方案会失败，再改为显式授权。最终四项都通过。

调度插入点与定时器回放入口仅在 `foundation-test` 下编译，见
[测试接口](../../ecraos/src/foundation_test.rs)。
[同步 smoke 的依赖配置](../../smoke-apps/sync-preemption/Cargo.toml)启用了该特性，
因此既有 CI 命令会自动运行新回归。测试通过指定交错和任务状态断言判断结果，
不依赖偶然命中的抢占窗口。

2026-11-11 对最终实现执行的检查结果如下：

| 检查 | 结果与范围 |
| --- | --- |
| 完整 QEMU smoke 矩阵 | 36/36 通过：三个应用 × 三种架构 × debug/release × 1/4 CPU |
| RustSBI 同步 smoke | 4/4 通过：RISC-V × debug/release × 1/4 CPU |
| `ecraos` host 单元测试 | 78 项通过 |
| Clippy | `ecraos` 和 `sync-preemption` 各在三种架构退出成功，仍有已有告警 |
| 格式检查 | 根工作区与 smoke 工作区通过 |
| Diff 检查 | `git diff --check` 通过 |
| AI 独立复审 | 对显式授权的最终实现未发现新的可操作问题 |

矩阵中的三个应用为 `default`、`sync-preemption`、`sleep-join`，
三种架构为 x86-64、RISC-V 64、AArch64。12 份同步 smoke 日志均核对了四个
回归成功标记和原有 smoke 成功标记。RustSBI 的四份同步日志也包含全部标记。

可从仓库根目录重新运行最终验证中的核心命令：

```bash
cargo xtask qemu \
    --crate smoke-apps/default,smoke-apps/sync-preemption,smoke-apps/sleep-join \
    --target x86_64-unknown-none,riscv64gc-unknown-none-elf,aarch64-unknown-none-softfloat \
    --profile debug,release --cpus 1,4 --timeout 20

# 先按 RustSBI 启动文档准备固件。
cargo xtask qemu --crate smoke-apps/sync-preemption \
    --target riscv64gc-unknown-none-elf \
    --profile debug,release --cpus 1,4 --timeout 30 \
    --qemu-arg=-bios \
    --qemu-arg=target/rustsbi/rustsbi-prototyper-dynamic.bin

cargo test --manifest-path Cargo.toml -p ecraos --lib --features host-test
```

固件准备方法见 [RustSBI 启动记录](2026-10-11-rustsbi-boot.md)。
以上验证覆盖了这些确定性交错及 QEMU smoke 场景，不构成对所有可能并发交错的穷举。
