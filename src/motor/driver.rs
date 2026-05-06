#[derive(Clone, Copy, Debug, Default)]
pub struct MotorOutput {
    pub normalized_duty: f32,
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
