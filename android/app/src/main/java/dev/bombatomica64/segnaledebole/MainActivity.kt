package dev.bombatomica64.segnaledebole

import android.hardware.input.InputManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.WindowManager
import androidx.activity.OnBackPressedCallback
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity

/**
 * SEGNALE DEBOLE on Android: GameActivity hosts `libsegnale_debole.so`
 * (Bevy's `android_main`, named by the `android.app.lib_name` meta-data).
 *
 * Input is a controller or mouse & keyboard; there are no touch controls.
 * Keyboards reach Bevy through GameActivity + winit as usual. Controllers and
 * the mouse do not (GameActivity forwards only touchscreen motion, winit turns
 * mouse input into touches and has no gamepads), so this activity intercepts
 * them and forwards them to the JNI bridge in `src/android_input.rs`, and
 * mirrors the game's cursor capture as Android pointer capture.
 */
class MainActivity : GameActivity(), InputManager.InputDeviceListener {

    companion object {
        init {
            // GameActivity also loads it from `android.app.lib_name`; loading
            // here first guarantees the JNI exports below resolve.
            System.loadLibrary("segnale_debole")
        }

        /** Pointer-capture poll period (ms). */
        private const val CAPTURE_POLL_MS = 100L

        /**
         * Right-stick axis pair from which candidate axes are centered sticks
         * (`[-1, 1]`-style range). Pure; mirrored and unit-tested in Rust as
         * `android_input::select_right_stick_pair`. Prefers `Z / RZ` (the
         * Android standard mapping), then `RX / RY` (many HID / Xbox-style
         * pads), else `null` (zero look) so uncentered trigger axes on `Z / RZ`
         * can never rotate the camera.
         */
        @JvmStatic
        fun selectRightStickPair(zRz: Boolean, rxRy: Boolean): Pair<Int, Int>? = when {
            zRz -> Pair(MotionEvent.AXIS_Z, MotionEvent.AXIS_RZ)
            rxRy -> Pair(MotionEvent.AXIS_RX, MotionEvent.AXIS_RY)
            else -> null
        }

        /** Whether `axis` reports a centered stick range on `device`'s joystick source. */
        @JvmStatic
        fun isCenteredStick(device: InputDevice, axis: Int): Boolean {
            val range = device.getMotionRange(axis, InputDevice.SOURCE_JOYSTICK)
            return range != null && range.min < -0.5f && range.max > 0.5f
        }
    }

    /** Per-device right-stick axis pair (resolved once a real device is seen). */
    private val rightStickAxes = HashMap<Int, Pair<Int, Int>?>()

    private fun rightStickPair(deviceId: Int, device: InputDevice?): Pair<Int, Int>? {
        if (device == null) return rightStickAxes[deviceId]
        return rightStickAxes.getOrPut(deviceId) {
            selectRightStickPair(
                zRz = isCenteredStick(device, MotionEvent.AXIS_Z) && isCenteredStick(device, MotionEvent.AXIS_RZ),
                rxRy = isCenteredStick(device, MotionEvent.AXIS_RX) && isCenteredStick(device, MotionEvent.AXIS_RY),
            )
        }
    }

    private external fun nativeGamepadConnected(deviceId: Int, vendorId: Int, productId: Int)
    private external fun nativeGamepadDisconnected(deviceId: Int)
    private external fun nativeGamepadKey(deviceId: Int, keyCode: Int, pressed: Boolean)
    private external fun nativeGamepadAxes(
        deviceId: Int,
        lx: Float, ly: Float, rx: Float, ry: Float,
        lt: Float, rt: Float, hatX: Float, hatY: Float,
    )
    private external fun nativeMouseMove(x: Float, y: Float)
    private external fun nativeMouseDelta(dx: Float, dy: Float)
    private external fun nativeMouseButtons(buttonState: Int)
    private external fun nativeMouseScroll(x: Float, y: Float)
    private external fun nativeResetInputs()
    private external fun nativeWantsPointerCapture(): Boolean

    private val handler = Handler(Looper.getMainLooper())
    private var inputManager: InputManager? = null

    private val capturePoll = object : Runnable {
        override fun run() {
            syncPointerCapture()
            handler.postDelayed(this, CAPTURE_POLL_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            window.attributes = window.attributes.also {
                it.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        enterImmersiveMode()

        // Back (gesture, nav bar, or a controller / mouse fallback) must never
        // kill a run: Esc / Start pause in-game. Home leaves the app.
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() = Unit
        })

        mSurfaceView?.let { view ->
            view.isFocusable = true
            view.isFocusableInTouchMode = true
            view.setOnCapturedPointerListener { _, event -> onCapturedPointer(event) }
        }

        inputManager = (getSystemService(INPUT_SERVICE) as InputManager).also {
            it.registerInputDeviceListener(this, handler)
        }
        for (id in InputDevice.getDeviceIds()) {
            onInputDeviceAdded(id)
        }
    }

    override fun onDestroy() {
        inputManager?.unregisterInputDeviceListener(this)
        handler.removeCallbacks(capturePoll)
        super.onDestroy()
    }

    override fun onResume() {
        super.onResume()
        enterImmersiveMode()
        handler.removeCallbacks(capturePoll)
        handler.post(capturePoll)
    }

    override fun onPause() {
        handler.removeCallbacks(capturePoll)
        mSurfaceView?.let { if (it.hasPointerCapture()) it.releasePointerCapture() }
        nativeResetInputs()
        super.onPause()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) {
            enterImmersiveMode()
            syncPointerCapture()
        } else {
            mSurfaceView?.let { if (it.hasPointerCapture()) it.releasePointerCapture() }
            nativeResetInputs()
        }
    }

    /** Sticky immersive fullscreen: system bars hidden, swipe shows them briefly. */
    private fun enterImmersiveMode() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    /** Request / release pointer capture to match the game's cursor capture. */
    private fun syncPointerCapture() {
        val view = mSurfaceView ?: return
        val want = hasWindowFocus() && nativeWantsPointerCapture()
        if (want && !view.hasPointerCapture()) {
            view.requestFocus()
            view.requestPointerCapture()
        } else if (!want && view.hasPointerCapture()) {
            view.releasePointerCapture()
        }
    }

    // ---------------------------------------------------------------- devices

    private fun isGamepad(device: InputDevice?): Boolean {
        val sources = device?.sources ?: return false
        return (sources and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD ||
            (sources and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
    }

    override fun onInputDeviceAdded(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId)
        if (isGamepad(device) && device != null && !device.isVirtual) {
            nativeGamepadConnected(deviceId, device.vendorId, device.productId)
        }
    }

    override fun onInputDeviceRemoved(deviceId: Int) {
        rightStickAxes.remove(deviceId)
        nativeGamepadDisconnected(deviceId)
    }

    // A remapped / reconfigured device re-resolves its right-stick axes.
    override fun onInputDeviceChanged(deviceId: Int) {
        rightStickAxes.remove(deviceId)
    }

    // ---------------------------------------------------------------- gamepad

    private fun isGamepadSource(source: Int): Boolean =
        (source and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD ||
            (source and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK

    private fun isGamepadKey(event: KeyEvent): Boolean {
        val code = event.keyCode
        val isDpad = code == KeyEvent.KEYCODE_DPAD_UP || code == KeyEvent.KEYCODE_DPAD_DOWN ||
            code == KeyEvent.KEYCODE_DPAD_LEFT || code == KeyEvent.KEYCODE_DPAD_RIGHT ||
            code == KeyEvent.KEYCODE_DPAD_CENTER
        if (isGamepadSource(event.source)) {
            return KeyEvent.isGamepadButton(code) || isDpad
        }
        // Some controllers emit D-pad keys tagged SOURCE_DPAD rather than
        // SOURCE_GAMEPAD; accept them when the originating device is a gamepad.
        return isDpad &&
            (event.source and InputDevice.SOURCE_DPAD) == InputDevice.SOURCE_DPAD &&
            isGamepad(event.device)
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (isGamepadKey(event)) {
            when (event.action) {
                KeyEvent.ACTION_DOWN -> if (event.repeatCount == 0) {
                    nativeGamepadKey(event.deviceId, event.keyCode, true)
                }
                KeyEvent.ACTION_UP -> nativeGamepadKey(event.deviceId, event.keyCode, false)
            }
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    private fun sendAxes(event: MotionEvent) {
        fun axis(a: Int) = event.getAxisValue(a)
        val pair = rightStickPair(event.deviceId, event.device)
        val rx = if (pair != null) axis(pair.first) else 0f
        val ry = if (pair != null) axis(pair.second) else 0f
        nativeGamepadAxes(
            event.deviceId,
            axis(MotionEvent.AXIS_X), axis(MotionEvent.AXIS_Y),
            rx, ry,
            maxOf(axis(MotionEvent.AXIS_LTRIGGER), axis(MotionEvent.AXIS_BRAKE)),
            maxOf(axis(MotionEvent.AXIS_RTRIGGER), axis(MotionEvent.AXIS_GAS)),
            axis(MotionEvent.AXIS_HAT_X), axis(MotionEvent.AXIS_HAT_Y),
        )
    }

    // ------------------------------------------------------------------ mouse

    /** Uncaptured mouse (menus): absolute position, buttons, wheel. */
    private fun sendMouse(event: MotionEvent) {
        nativeMouseMove(event.x, event.y)
        nativeMouseButtons(event.buttonState)
        if (event.actionMasked == MotionEvent.ACTION_SCROLL) {
            nativeMouseScroll(event.getAxisValue(MotionEvent.AXIS_HSCROLL), event.getAxisValue(MotionEvent.AXIS_VSCROLL))
        }
    }

    /** Captured mouse (in a run): relative motion, buttons, wheel. */
    private fun onCapturedPointer(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_SCROLL) {
            nativeMouseScroll(event.getAxisValue(MotionEvent.AXIS_HSCROLL), event.getAxisValue(MotionEvent.AXIS_VSCROLL))
        } else {
            var dx = event.x
            var dy = event.y
            for (i in 0 until event.historySize) {
                dx += event.getHistoricalX(i)
                dy += event.getHistoricalY(i)
            }
            if (dx != 0f || dy != 0f) nativeMouseDelta(dx, dy)
        }
        nativeMouseButtons(event.buttonState)
        return true
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        if (isGamepadSource(event.source) && event.actionMasked == MotionEvent.ACTION_MOVE) {
            sendAxes(event)
            return true
        }
        if (event.isFromSource(InputDevice.SOURCE_MOUSE)) {
            sendMouse(event)
            return true
        }
        return super.dispatchGenericMotionEvent(event)
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        // Mouse clicks / drags arrive as touch events; keep them out of
        // GameActivity (winit would report them as touches).
        if (event.isFromSource(InputDevice.SOURCE_MOUSE)) {
            sendMouse(event)
            return true
        }
        return super.dispatchTouchEvent(event)
    }
}
