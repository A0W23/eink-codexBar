"""PROTOTYPE — pure reducer for the hook lifecycle question.

Question: do the hook enums emitted by this Codex build provide enough
transitions to represent running, needs-user, ready-for-review, failure, and
interruption without reading content-bearing fields?
"""


def initial_state():
    return {
        "phase": "unknown",
        "waiting": False,
        "last_event": None,
        "reliability": "unavailable",
    }


def reduce_event(state, record):
    next_state = dict(state)
    event = record["event"]
    next_state["last_event"] = event

    if event == "user_prompt_submit":
        next_state.update(phase="executing", waiting=False, reliability="inferred")
    elif event == "permission_request":
        next_state.update(phase="needs_user", waiting=True, reliability="authoritative")
    elif event == "post_tool_use" and state["waiting"]:
        next_state.update(phase="executing", waiting=False, reliability="inferred")
    elif event == "stop":
        next_state.update(phase="turn_ended", waiting=False, reliability="inferred")
    elif event == "session_end":
        next_state.update(phase="session_ended", waiting=False, reliability="authoritative")

    result = record.get("result")
    if result == "failure":
        next_state.update(phase="failed", waiting=False, reliability="authoritative")
    elif result == "interrupted":
        next_state.update(phase="interrupted", waiting=False, reliability="authoritative")
    return next_state
