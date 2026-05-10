Remaining delta I can do next:

Move the EN/PH low-level hardware code out of task.rs into a concrete reusable driver in driver.rs (currently still generic placeholder).
Add CLI tuning/config commands for runtime gain/deadband/duty tuning.
Add explicit fault-recovery command flow (clear fault, re-home, resume).