I'm implementing on a dfrobot Romeo ESP32-C3-MINI-1 Motor & Servo Control Board. This board has two motor control channels and a number of IO channels. I would like to plan the implementation of a motor control task specifically for a motor which moves along a rack and pinion gear to have a position between 0 & 100 percent. This controller should take commands from other tasks and return position to those tasks. I would like to be able to specify a target position or target velocity.

The actual setup should be modular to take configuration for the GPIOS to use. The board has two DRV8220DSGR which can drive the motors. I also have limit switches at both ends (top & bottom) of the rack and pinion. Additionally the motor I'm using has an encoder with two pins.

Please could you present a plan that includes the appropriate task / driver / cli commands, etc. for this. Assume that the configuration is passed in.

For this specific instance we will use IO0 and IO1 to drive the motor. The data sheet says M1: GPIO1 (direction control), GPIO0 (PWM control). The top limit switch will be connected on IO6, the bottom limit switch is connected on IO7. The encoder channel 1 is connected go IO21 and channel 2 is connected to IO20.

The schematic shows the motor controler has the mode pin pulled up which indicates that it is running in EN/PH mode.  When the motor is not in motion the driver should set the pins such that the motor is not being powered so it doesn't heat up. 

Ensure your plan also cover the testing and tuning procedure. Design the API in such a way that it can be used with a PID in the future.

Ask any questions that you have.