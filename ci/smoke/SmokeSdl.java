// SDL3 counterpart of Smoke.java: Minecraft 26.x opens its window with SDL3 (LWJGL 3.4)
// instead of GLFW. Opens an OpenGL 3.2 core window with SDL3 and swaps frames, so the
// agent's SDL3 detection can be smoke-tested. Java 8 source: javac --release 8.
//
// Usage: java -cp <lwjgl jars incl. lwjgl-sdl>;<classes> SmokeSdl [FRAMES]
// Prints "SMOKE OK frames=N" and exits 0, or "SMOKE FAIL: <reason>" and exits 1.

import org.lwjgl.Version;
import org.lwjgl.opengl.GL;
import org.lwjgl.sdl.SDL_Event;

import static org.lwjgl.opengl.GL11.*;
import static org.lwjgl.sdl.SDLError.SDL_GetError;
import static org.lwjgl.sdl.SDLEvents.SDL_PollEvent;
import static org.lwjgl.sdl.SDLInit.*;
import static org.lwjgl.sdl.SDLVideo.*;

public final class SmokeSdl {
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
        for (String arg : args) {
            if (arg.startsWith("--")) {
                continue; // Smoke.java's flags; not supported here
            }
            try {
                frames = Integer.parseInt(arg);
            } catch (NumberFormatException e) {
                throw new Fail("unknown argument " + arg);
            }
            if (frames < 1) {
                throw new Fail("frame count must be positive: " + arg);
            }
        }
        System.out.println("LWJGL " + Version.getVersion());
        if (!SDL_Init(SDL_INIT_VIDEO)) {
            throw new Fail("SDL_Init failed: " + SDL_GetError());
        }
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 3);
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 2);
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_CORE);
        long window = SDL_CreateWindow("reminedog smoke (SDL3)", 854, 480, SDL_WINDOW_OPENGL);
        if (window == 0) {
            throw new Fail("SDL_CreateWindow failed: " + SDL_GetError());
        }
        long context = SDL_GL_CreateContext(window);
        if (context == 0) {
            throw new Fail("SDL_GL_CreateContext failed: " + SDL_GetError());
        }
        SDL_GL_MakeCurrent(window, context);
        SDL_GL_SetSwapInterval(0);
        GL.createCapabilities();
        System.out.println("GL_VERSION " + glGetString(GL_VERSION));
        System.out.println("GL_RENDERER " + glGetString(GL_RENDERER));

        SDL_Event event = SDL_Event.calloc();
        try {
            for (int frame = 0; frame < frames; frame++) {
                glClearColor(0.2f, 0.4f, 0.6f, 1f);
                glClear(GL_COLOR_BUFFER_BIT);
                if (!SDL_GL_SwapWindow(window)) {
                    throw new Fail("frame " + frame + ": SDL_GL_SwapWindow failed: " + SDL_GetError());
                }
                while (SDL_PollEvent(event)) {
                    // Drain the queue, as a game loop does.
                }
                int error = glGetError();
                if (error != GL_NO_ERROR) {
                    throw new Fail(String.format("frame %d: GL error 0x%X", frame, error));
                }
            }
        } finally {
            event.free();
        }
        SDL_GL_DestroyContext(context);
        SDL_DestroyWindow(window);
        SDL_Quit();
        System.out.println("SMOKE OK frames=" + frames);
    }
}
