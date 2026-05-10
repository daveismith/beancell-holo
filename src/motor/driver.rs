#[derive(Clone, Copy, Debug, Default)]
pub struct MotorOutput {
    pub duty_cycle: f32, // -1.0 (full reverse) to +1.0 (full forward)
}

pub trait MotorDriver {
    fn apply_output(&mut self, output: MotorOutput);
    fn coast(&mut self);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopMotorDriver {
    pub last_output: MotorOutput,
}

impl MotorDriver for NoopMotorDriver {
    fn apply_output(&mut self, output: MotorOutput) {
        self.last_output = output;
    }

    fn coast(&mut self) {
        self.last_output = MotorOutput::default();
    }
}
