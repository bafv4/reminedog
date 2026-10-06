// Minimal stand-in for Minecraft's LWJGL3/GLFW window and frame loop, used to smoke-test the
// reminedog agent (-agentpath:) without the game. Java 8 source: javac --release 8.
//
// Usage: java -cp <lwjgl jars>;<classes> Smoke [FRAMES] [--legacy] [--readback] [--screenshot]
//                                            [--seconds=N] [--capture] [--mc [--sodium]] [--world=NAME]
//                                            [--f3c-events] [--f3c-refuse]
//                                            [--screen-key=KEY [--watch-keys=KEY,...]]
//   FRAMES      frames to render (default 120)
//   --legacy    default window hints, like Minecraft 1.13-1.16 (else 3.2 core, like 1.17+)
//   --readback  after the last swap, print "PIXEL x y r g b" (front buffer) and
//               "PIXEL_BACK x y r g b" (back buffer) for a few window coordinates
//   --seconds=N   run for N seconds at about 60 fps instead of a frame count (for input tests)
//   --capture     grab the cursor like Minecraft in game (GLFW_CURSOR_DISABLED), with raw
//                 mouse motion where supported (Minecraft's "Raw Input" setting, on by default)
//   --mc          render like Minecraft: into its own framebuffer at the size the framebuffer
//                 size callback reports, then copied into the window
//   --sodium      with --mc, set the viewport like Minecraft with Sodium, which skips a
//                 glViewport equal to the last one (SmokeImage.viewport)
//   --screenshot  after the last swap, save the back buffer as smoke-screenshot.png in the
//                 current directory (diagnostic: what an overlay drew, where the driver keeps
//                 the back buffer after a swap, as Wine + llvmpipe does)
//   --world=NAME  at start, write saves/NAME/session.lock and a logs/latest.log with the
//                 integrated server's start line into the current directory, as Minecraft
//                 does when it opens a singleplayer world (so the agent knows the world)
//   --f3c-events  F3 is held when its key events say so, like Minecraft 1.21.9+ (else when
//                 glfwGetKey(F3) says so as a key is handled, like 1.16)
//   --f3c-refuse  F3+C copies nothing, like a world with reduced debug info
//   --screen-key=KEY  pressing the key with this GLFW key code (e.g. 69 for E) opens a fake
//                 screen, which releases the cursor ("SCREEN open"), or closes it, which grabs
//                 the cursor again like Minecraft (SmokeImage.Screen). The run starts in game
//                 with --capture, else on a screen
//   --watch-keys=KEY,...  when the screen closes, print "SETALL <key>=<0|1> ..." for these GLFW
//                 key codes as glfwGetKey reads them just before the cursor is grabbed, as
//                 Minecraft's KeyMapping.setAll does
// SMOKE_VERBOSE=1 prints what the "game" receives: "KEY key scancode action mods",
// "BUTTON button action mods" and "FOCUS 0|1" (GLFW's callback arguments).
// Every frame it checks that the game's WGL context is still current and that there is no
// GL error, so an agent that leaks state into the game's context fails the run. The debug
// keys act like Minecraft's (SmokeImage.DebugKeys): pressing C while F3 is held copies a
// Minecraft-style location with glfwSetClipboardString ("F3C copied"), and releasing F3
// without a handled debug key toggles the debug overlay; at the end it prints
// "F3C STATE overlay=<on|off> modifier=<up|down> copies=N".
// Prints "SMOKE OK frames=N" and exits 0, or "SMOKE FAIL: <reason>" and exits 1.

import java.nio.ByteBuffer;

import org.lwjgl.BufferUtils;
import org.lwjgl.Version;
import org.lwjgl.glfw.Callbacks;
import org.lwjgl.glfw.GLFWErrorCallback;
import org.lwjgl.glfw.GLFWNativeWGL;
import org.lwjgl.opengl.GL;
import org.lwjgl.opengl.WGL;
import org.lwjgl.system.Platform;

import static org.lwjgl.glfw.GLFW.*;
import static org.lwjgl.opengl.GL11.*;

public final class Smoke {
    // Clear colour, chosen to be exact in 8 bits: RGB 51 102 153.
    private static final float CLEAR_R = 0.2f, CLEAR_G = 0.4f, CLEAR_B = 0.6f;

    private static int keys, chars, buttons, cursorMoves, scrolls, glfwErrors;
    // F3 as its key events left it (--f3c-events).
    private static boolean f3Down;
    private static final StringBuilder typed = new StringBuilder();
    // SMOKE_VERBOSE=1 prints every key, mouse button and focus event the "game" receives.
    private static final boolean VERBOSE = System.getenv("SMOKE_VERBOSE") != null;
    private static String lastGlfwError = "none";

    private static final class Fail extends RuntimeException {
        private static final long serialVersionUID = 1L;

        Fail(String reason) {
            super(reason);
        }
    }

    public static void main(String[] args) {
        int code;
        try {
            run(args);
            code = 0;
        } catch (Fail e) {
            System.out.println("SMOKE FAIL: " + e.getMessage());
            code = 1;
        } catch (Throwable t) {
            System.out.println("SMOKE FAIL: " + t);
            t.printStackTrace(System.out);
            code = 1;
        }
        System.out.flush();
        System.exit(code);
    }

    private static void run(String[] args) {
        int frames = 120;
        boolean legacy = false, readback = false, screenshot = false, capture = false, mc = false;
        boolean f3cEvents = false, f3cRefuse = false;
        String world = null;
        double seconds = 0;
        int screenKey = 0;
        int[] watchKeys = null;
        for (String arg : args) {
            if (arg.startsWith("--seconds=")) {
                seconds = Double.parseDouble(arg.substring("--seconds=".length()));
            } else if (arg.startsWith("--screen-key=")) {
                screenKey = SmokeImage.keyCode(arg, GLFW_KEY_SPACE, GLFW_KEY_LAST);
            } else if (arg.startsWith("--watch-keys=")) {
                watchKeys = SmokeImage.keyCodes(arg, GLFW_KEY_SPACE, GLFW_KEY_LAST);
            } else if (arg.startsWith("--world=")) {
                world = arg.substring("--world=".length());
            } else if (arg.equals("--f3c-events")) {
                f3cEvents = true;
            } else if (arg.equals("--f3c-refuse")) {
                f3cRefuse = true;
            } else if (arg.equals("--capture")) {
                capture = true;
            } else if (arg.equals("--mc")) {
                mc = true;
            } else if (arg.equals("--sodium")) {
                SmokeImage.sodium = true;
            } else if (arg.equals("--legacy")) {
                legacy = true;
            } else if (arg.equals("--readback")) {
                readback = true;
            } else if (arg.equals("--screenshot")) {
                screenshot = true;
            } else {
                try {
                    frames = Integer.parseInt(arg);
                } catch (NumberFormatException e) {
                    throw new Fail("unknown argument " + arg);
                }
                if (frames < 1) {
                    throw new Fail("frame count must be positive: " + arg);
                }
            }
        }
        if (watchKeys != null && screenKey == 0) {
            throw new Fail("--watch-keys needs --screen-key");
        }
        if (SmokeImage.sodium && !mc) {
            throw new Fail("--sodium needs --mc");
        }
        long t0 = System.nanoTime();
        System.out.println("LWJGL " + Version.getVersion());
        if (world != null) {
            SmokeImage.writeWorld(world);
        }

        // Minecraft installs an error callback before glfwInit.
        GLFWErrorCallback errorCallback = GLFWErrorCallback.create((error, description) -> {
            glfwErrors++;
            lastGlfwError = String.format("0x%X %s", error, GLFWErrorCallback.getDescription(description));
            System.out.println("GLFW ERROR " + lastGlfwError);
        });
        glfwSetErrorCallback(errorCallback);
        if (!glfwInit()) {
            throw new Fail("glfwInit failed (last GLFW error: " + lastGlfwError + ")");
        }
        System.out.println("GLFW " + glfwGetVersionString());

        glfwDefaultWindowHints();
        if (!legacy) {
            // Window hints of Minecraft 1.17+.
            glfwWindowHint(GLFW_CLIENT_API, GLFW_OPENGL_API);
            glfwWindowHint(GLFW_CONTEXT_CREATION_API, GLFW_NATIVE_CONTEXT_API);
            glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
            glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 2);
            glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);
            glfwWindowHint(GLFW_OPENGL_FORWARD_COMPAT, GLFW_TRUE);
        }
        long window = glfwCreateWindow(854, 480, "reminedog smoke", 0, 0);
        if (window == 0) {
            throw new Fail("glfwCreateWindow failed (last GLFW error: " + lastGlfwError + ")");
        }

        // Minecraft makes the context current and creates GL capabilities in its Window
        // constructor, and registers the input callbacks afterwards.
        glfwMakeContextCurrent(window);
        GL.createCapabilities();
        glfwSwapInterval(0); // vsync off keeps runs short
        System.out.println("GL_VERSION " + glGetString(GL_VERSION));
        System.out.println("GL_RENDERER " + glGetString(GL_RENDERER));
        System.out.println("GL_VENDOR " + glGetString(GL_VENDOR));

        final boolean events = f3cEvents;
        final SmokeImage.DebugKeys debugKeys = new SmokeImage.DebugKeys(f3cRefuse,
            () -> glfwSetClipboardString(window, SmokeImage.DebugKeys.F3C_TEXT));
        final SmokeImage.Screen screen = screenKey == 0 ? null
            : new SmokeImage.Screen(screenKey, !capture, watchKeys != null ? watchKeys : new int[0],
                key -> glfwGetKey(window, key) == GLFW_PRESS,
                () -> setCursor(window, GLFW_CURSOR_NORMAL), () -> setCursor(window, GLFW_CURSOR_DISABLED));
        glfwSetKeyCallback(window, (w, key, scancode, action, mods) -> {
            keys++;
            if (VERBOSE) {
                System.out.printf("KEY %d %d %d %d%n", key, scancode, action, mods);
            }
            // Minecraft 1.16 asks glfwGetKey whether F3 is held; 1.21.9+ follows F3's key events.
            // Both read the crash key's real state with glfwGetKey.
            boolean f3Held = events ? f3Down : glfwGetKey(w, GLFW_KEY_F3) == GLFW_PRESS;
            debugKeys.keyEvent(glfwGetKey(w, GLFW_KEY_C) == GLFW_PRESS, f3Held);
            if (key == GLFW_KEY_F3 && action == GLFW_RELEASE) {
                debugKeys.modifierReleased();
            } else if (action != GLFW_RELEASE && f3Held) {
                debugKeys.debugKey(key == GLFW_KEY_C);
            }
            if (key == GLFW_KEY_F3) {
                f3Down = action != GLFW_RELEASE;
            }
            if (screen != null && action == GLFW_PRESS) {
                screen.keyPressed(key);
            }
        });
        glfwSetCharModsCallback(window, (w, codepoint, mods) -> {
            chars++;
            typed.appendCodePoint(codepoint);
        });
        glfwSetMouseButtonCallback(window, (w, button, action, mods) -> {
            buttons++;
            if (VERBOSE) {
                System.out.printf("BUTTON %d %d %d%n", button, action, mods);
            }
        });
        glfwSetCursorPosCallback(window, (w, x, y) -> cursorMoves++);
        glfwSetScrollCallback(window, (w, dx, dy) -> scrolls++);
        final int[] fbSize = new int[2];
        {
            int[] w = new int[1], h = new int[1];
            glfwGetFramebufferSize(window, w, h);
            fbSize[0] = w[0];
            fbSize[1] = h[0];
        }
        glfwSetFramebufferSizeCallback(window, (w, width, height) -> {
            fbSize[0] = width;
            fbSize[1] = height;
        });
        // Minecraft follows the window's focus too (the agent learns of focus loss through it).
        glfwSetWindowFocusCallback(window, (w, focused) -> {
            if (VERBOSE) {
                System.out.println("FOCUS " + (focused ? 1 : 0));
            }
        });
        SmokeImage.MainTarget mainTarget = mc ? new SmokeImage.MainTarget() : null;
        if (capture) {
            glfwSetInputMode(window, GLFW_CURSOR, GLFW_CURSOR_DISABLED);
        }
        if ((capture || screen != null) && glfwRawMouseMotionSupported()) {
            glfwSetInputMode(window, GLFW_RAW_MOUSE_MOTION, GLFW_TRUE);
        }

        // The agent swaps GL contexts inside the swap; make sure ours is current afterwards.
        long wglContext = Platform.get() == Platform.WINDOWS ? GLFWNativeWGL.glfwGetWGLContext(window) : 0;
        int glError = glGetError();
        if (glError != GL_NO_ERROR) {
            throw new Fail(String.format("GL error 0x%X after setup", glError));
        }

        long t1 = System.nanoTime();
        long until = t1 + (long) (seconds * 1e9);
        int frame = 0;
        for (; seconds > 0 ? System.nanoTime() < until : frame < frames; frame++) {
            if (seconds > 0) {
                sleep(15);
            }
            if (mainTarget != null) {
                mainTarget.render(fbSize[0], fbSize[1]);
                mainTarget.present();
            } else {
                glClearColor(CLEAR_R, CLEAR_G, CLEAR_B, 1f);
                glClear(GL_COLOR_BUFFER_BIT);
                SmokeImage.drawPattern(854, 480);
            }
            glfwSwapBuffers(window);
            glfwPollEvents();
            if (wglContext != 0 && WGL.wglGetCurrentContext() != wglContext) {
                throw new Fail("frame " + frame + ": WGL context is no longer current after swap");
            }
            glError = glGetError();
            if (glError != GL_NO_ERROR) {
                throw new Fail(String.format("frame %d: GL error 0x%X", frame, glError));
            }
        }
        long t2 = System.nanoTime();

        if (readback) {
            readback(window);
        }
        if (screenshot) {
            screenshot(window, "smoke-screenshot.png");
        }
        frames = frame;
        System.out.printf("EVENTS key=%d char=%d button=%d cursor=%d scroll=%d glfw_errors=%d%n",
            keys, chars, buttons, cursorMoves, scrolls, glfwErrors);
        System.out.println("TYPED " + typed);
        debugKeys.printState(f3cEvents ? f3Down : glfwGetKey(window, GLFW_KEY_F3) == GLFW_PRESS);
        System.out.printf("TIMING setup_ms=%d loop_ms=%d fps=%.0f%n",
            (t1 - t0) / 1000000, (t2 - t1) / 1000000, frames * 1e9 / Math.max(1, t2 - t1));

        // Minecraft's shutdown order: free the window callbacks (LWJGL frees whatever the
        // glfwSet*Callback calls return, so hooks must hand back the game's pointers).
        Callbacks.glfwFreeCallbacks(window);
        glfwDestroyWindow(window);
        glfwTerminate();
        glfwSetErrorCallback(null);
        errorCallback.free();
        System.out.println("SMOKE OK frames=" + frames);
    }

    // Diagnostic only: after the last swap, print pixels at a few window coordinates from the
    // front buffer ("PIXEL") and the back buffer ("PIXEL_BACK"). Under Wine + Mesa llvmpipe the
    // front buffer reads back black, while the back buffer still holds the presented frame
    // (copy swap); on real drivers the back buffer is undefined after a swap.
    private static void readback(long window) {
        int[] winW = new int[1], winH = new int[1], fbW = new int[1], fbH = new int[1];
        glfwGetWindowSize(window, winW, winH);
        glfwGetFramebufferSize(window, fbW, fbH);
        glFinish();
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        System.out.printf("CLEAR %d %d %d%n",
            Math.round(CLEAR_R * 255), Math.round(CLEAR_G * 255), Math.round(CLEAR_B * 255));
        ByteBuffer rgba = BufferUtils.createByteBuffer(4);
        int[][] points = {{40, 40}, {120, 80}, {winW[0] / 2, winH[0] / 2}};
        int[] buffers = {GL_FRONT, GL_BACK};
        for (int buffer : buffers) {
            glReadBuffer(buffer);
            for (int[] p : points) {
                // Window coordinates have a top-left origin; GL's are bottom-left.
                int x = p[0] * fbW[0] / Math.max(1, winW[0]);
                int y = fbH[0] - 1 - p[1] * fbH[0] / Math.max(1, winH[0]);
                glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
                System.out.printf("%s %d %d %d %d %d%n", buffer == GL_FRONT ? "PIXEL" : "PIXEL_BACK",
                    p[0], p[1], rgba.get(0) & 0xFF, rgba.get(1) & 0xFF, rgba.get(2) & 0xFF);
            }
        }
        glReadBuffer(GL_BACK);
        int glError = glGetError();
        if (glError != GL_NO_ERROR) {
            throw new Fail(String.format("GL error 0x%X during readback", glError));
        }
    }

    private static void screenshot(long window, String path) {
        int[] w = new int[1], h = new int[1];
        glfwGetFramebufferSize(window, w, h);
        SmokeImage.saveBackBuffer(w[0], h[0], path);
    }

    // Minecraft's InputConstants.grabOrReleaseMouse: the cursor to the window's centre, then
    // the cursor mode.
    private static void setCursor(long window, int mode) {
        int[] w = new int[1], h = new int[1];
        glfwGetWindowSize(window, w, h);
        glfwSetCursorPos(window, w[0] / 2.0, h[0] / 2.0);
        glfwSetInputMode(window, GLFW_CURSOR, mode);
    }

    private static void sleep(long millis) {
        try {
            Thread.sleep(millis);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }
}
