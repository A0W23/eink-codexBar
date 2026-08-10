# Codex Dashboard for ZECTRIX

A shared language for representing Codex work on a small, persistent status display.

## Language

**Task**:
A top-level Codex conversation representing one continuing unit of work. A task can contain many execution turns, subagents, and plan steps.
_Avoid_: Prompt, turn, subagent

**Execution turn**:
One period of work initiated within a task by user input. Finishing an execution turn does not by itself finish the task.
_Avoid_: Task, conversation

**Task activity**:
The lower dashboard section showing the latest supported execution state for each visible task. A task has only one displayed activity state at a time in the MVP.
_Avoid_: Attention inbox, task history, plan

Ended activity states (`本轮完成`, `失败`, and `已中断`) remain eligible for display for 24 hours. A new execution turn for the same task replaces its previous ended state immediately with `执行中`.

The MVP displays at most three task activities. Selection priority is `执行中`, then `失败`, then `已中断`, then `本轮完成`; items within the same state are ordered by most recent activity. When additional eligible tasks exist, the section shows `另有 N 项`.

Task titles are visible by default so users can identify each activity. Setup must disclose before the first push that rendered titles are uploaded to ZECTRIX Cloud. Users can enable a privacy mode that hides titles while retaining states and counts. Project names, prompts, responses, reasoning, tool arguments, and plan text are never displayed in the MVP.

**Running** (`执行中`):
The task has a fresh execution-start or tool-activity signal and no corresponding end signal. Because Codex Desktop does not expose an attachable authoritative endpoint, this state requires a freshness timeout and becomes stale rather than remaining active indefinitely.
_Avoid_: Loaded, open, unfinished task

**Turn completed** (`本轮完成`):
The task's latest observed execution turn ended normally. It does not mean the whole task was accepted, archived, unread, or completed, and it must not claim to mirror the Desktop blue dot.
_Avoid_: Completed task, ready for review, unread

**Failed turn** (`失败`):
The latest observed execution turn ended with an explicit recorded error. The error content is not required or displayed.
_Avoid_: Interrupted, incomplete task

**Interrupted turn** (`已中断`):
The latest observed execution turn was explicitly aborted as interrupted rather than ending normally. It is distinct from a recorded failure.
_Avoid_: Failed, paused, needs user

**Subagent**:
A delegated worker operating within a task. It contributes progress to its parent task and is not displayed as an independent task.
_Avoid_: Task

**Plan step**:
One progress item within a task's current plan. It describes internal progress rather than an independent task.
_Avoid_: Task, subtask

**Ready for review**:
A task whose latest execution turn has ended and is waiting for the user to inspect the result. It is not yet a completed task.
_Avoid_: Completed, done

**Needs user**:
A task whose active execution turn cannot continue until the user provides input or approval. It takes priority over every other task on the dashboard.
_Avoid_: Idle, failed

**Completed task**:
A task the user has explicitly accepted as finished or archived. An execution turn ending does not make its task completed.
_Avoid_: Ready for review, idle

**Attention set**:
The top-level tasks that still need awareness: waiting for the user, running, ready for review, failed, or interrupted. Completed and ordinarily idle historical tasks are excluded.
_Avoid_: Task history, all tasks

The complete attention set is not an MVP capability on the tested Codex Desktop build. The MVP task activity section supports `执行中`, `本轮完成`, `失败`, and `已中断`; it omits `待你`, authoritative `检查`, and plan progress until a validated source exists.

**Quota window**:
A time-bounded ChatGPT Codex allowance reported as used and remaining percentages with a reset time. It is distinct from API billing and local token counts.
_Avoid_: Token usage, API spend, balance

**Reset credit**:
An available entitlement that can reset eligible Codex quota windows before their scheduled reset. A count of zero is omitted from the dashboard.
_Avoid_: Quota, API credit, token balance

**Plugin**:
The user-installed Codex distribution unit that owns setup, updates, diagnostics, hooks, and the bundled companion.
_Avoid_: Companion, standalone app

**Companion**:
The plugin's background worker that observes Codex state and publishes the dashboard. It is an internal runtime component, not a separately installed product.
_Avoid_: Plugin, app

**Last known state**:
The most recent successfully observed dashboard data retained when a source becomes unavailable. It must be marked as potentially stale rather than replaced with zeros or an empty task list.
_Avoid_: Current state, empty state
