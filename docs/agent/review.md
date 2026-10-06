# 代码评审：剩余待办

> 评审快照 2026-10-06（版本 `0.0.64`），覆盖 `crates/` 全部 21 个 `.rs`（4540 行）。
> 原报告共八节，§八 的落地顺序 1–6 已全部完成（`aa444aa`、`0eaf866`、`c8f2c5b`、
> `5e1857a`、`2ffa593`、`48798d5`、`ef1c575`），下面只剩三条当时没排进那六步的。
> 条目处理完就删掉对应段落，整份文件清空后可以删除。规则编号对应 `rust-skills`。

---

## 一、性能

### PERF-6 等待用 1 ms 轮询（`walk.rs`）

```rust
guard = shared.cvar.wait_timeout(guard, Duration::from_millis(1)).unwrap_or_else(..).0;
```

队列空、还有目录在途时，每个等待线程每秒醒 1000 次（8 线程 = 8000 次/秒）。

**为什么可以去掉超时**：所有状态变更都在同一把 mutex 下，`notify_all` 覆盖了全部会让
等待者前进的变更点（push 子目录、`pending` 归零），等待者被唤醒后也一定重新持锁复查 ——
不存在丢失唤醒的窗口。改成无条件 `wait(guard)` 即可。

> 保留超时的唯一理由是「万一有漏 notify 就死锁」。如果要保留这份保险，把 1 ms 放到
> 50~100 ms：抖动成本降两个数量级，同时仍能在漏唤醒时脱困。

### PERF-7 待观察（不建议现在动）

- `deduplicated_total`（`scan.rs`）与 `drop_apps_nested_in_identified_roots`（`group.rs`）
  都是 O(n²) 的嵌套包含判断。n 是**应用条数**（几十到几百），不是候选数，实际开销可忽略。
  真到几千条再改成「按路径长度升序 + 只与已接受的顶层根比」。
- `filter::allows_path` 对每个目录做 O(depth × (roots + excludes)) 的组件遍历。这是真热路径，
  但当前基线（[`performance.md`](performance.md)）已经达标，属于「先测量再动」。

---

## 二、unsafe 与 2024 edition

**做得好的**：每一处 `unsafe` 都有 `// SAFETY:` 且写清了不变量；`OwnedHandle` RAII 管 Win32
句柄；`ReplyWindow` 的 `Arc::into_raw`/`from_raw` 配平正确（create 里 +1、Drop 里 -1 + 字段
自身 -1）；`to_wide` / `to_wide_z` 严格分离并有测试钉住。

`&mut x as *mut _` → `&raw mut x`、`try_into().unwrap()` → `.expect(..)`、`_wparam` 改名
都已处理，还剩两条：

- **没跑 Miri**（`unsafe-miri-ci`）：合理 —— 这些 unsafe 全是 Win32 调用，Miri 跑不了。
  但 `is_executable_magic`、`read_u32`、`read_utf16_z` 这些**纯字节解析**是可以进 Miri 的，
  值得单开一个 `cfg(miri)` 的测试入口。
- **锁毒化的处理仍不一致**：core 侧已经统一成 `unwrap_or_else(PoisonError::into_inner)`，
  但 `icon.rs:15,21` 还有两处 `.lock().unwrap()`。生产代码里的 `unwrap` 会 panic
  （`err-no-unwrap-prod`）—— 图标取不到只是少个图标，不该崩掉整个 GUI。