# S04 cancellation fencing

`s04_cancelled_attempt_cannot_install_a_late_result` holds the fake provider response after dispatch, cancels the running job through the authenticated service, then releases the late response. The durable job and attempt remain cancelled and no proposal batch is created.
