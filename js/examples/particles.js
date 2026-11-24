// Renders 10,000 GPU particles with tgpu.
// Run: cargo run -- js/examples/particles.js

KetteTools.evalFile("kette/std/startup.ktt");
KetteTools.evalFile("kette/vendor/tgpu.ktt");
KetteTools.evalFile("kette/vendor/glfw.ktt");

const tgpu = globalThis.Modules.vendor.tgpu;
const glfw = globalThis.Modules.vendor.glfw;
const sys = globalThis.Modules.sys;
const ffi = globalThis.Modules.ffi;
const random = globalThis.Modules.random;

const PARTICLE_COUNT = 10000;
const FLOATS_PER_PARTICLE = 8;
const PARTICLE_BYTES = PARTICLE_COUNT * FLOATS_PER_PARTICLE * 4;
const SHADER_PATH = "kette/examples/tgpu/shader.slang";

function ceil_div(value, divisor) {
    const adjusted = value + divisor - 1;
    return (adjusted - (adjusted % divisor)) / divisor;
}


const glfwWindow = glfw.Window.new("tgpu particles (JS)", 800, 600, {
    clientApi: glfw.ClientApi.NONE,
});

const { width: startWidth, height: startHeight } = glfwWindow.framebufferSize();
if (startWidth <= 0 || startHeight <= 0) {
    throw "window has no framebuffer";
}

const gpu = tgpu.GPU.new();
const queue = gpu.queue;

// Mouse position in framebuffer coordinates, written by the GLFW callback.
const mouse = { x: 0, y: 0 };
glfwWindow.onCursorFramebuffer((x, y) => {
    mouse.x = x;
    mouse.y = glfwWindow.framebufferSize().height - y;
});
glfwWindow.onKey((key, action, mods) => {
    if (action === glfw.Action.press && key === glfw.Key.escape) {
        glfwWindow.setShouldClose(true);
    }
});

const swapchain = gpu.swapchain({
    window: glfwWindow.tgpuWindow(),
    width: startWidth,
    height: startHeight,
    imageCount: 3,
    presentMode: tgpu.PresentMode.fifo,
});

let extent = gpu.swapchainExtent(swapchain);
const colorFormat = tgpu.swapchainFormat(swapchain);

const particleData = ffi.array(ffi.f32, PARTICLE_COUNT * FLOATS_PER_PARTICLE).new();
random.seed(1);
for (let particleIndex = 0; particleIndex < PARTICLE_COUNT; particleIndex++) {
    const base = particleIndex * FLOATS_PER_PARTICLE;
    particleData.set(base + 0, random.unit() * extent.width);
    particleData.set(base + 1, random.unit() * extent.height);
    particleData.set(base + 2, (random.unit() * 2.0 - 1.0) * 30.0);
    particleData.set(base + 3, (random.unit() * 2.0 - 1.0) * 30.0);
    particleData.set(base + 4, 0.5 + random.unit() * 0.5);
    particleData.set(base + 5, 0.5 + random.unit() * 0.5);
    particleData.set(base + 6, 0.5 + random.unit() * 0.5);
    particleData.set(base + 7, 1.0);
}

const particleBuffer = gpu.buffer({
    size: PARTICLE_BYTES,
    usage: tgpu.BufferUsage.storage + tgpu.BufferUsage.copyDst,
    memory: tgpu.MemoryPreset.dynamic,
    hostAccess: tgpu.HostAccess.readWriteRandom,
    data: particleData,
});
particleData.free();

function createPresentImage(width, height) {
    return gpu.presentImage({
        image: {
            format: colorFormat,
            width,
            height,
            usage: tgpu.ImageUsage.copySrc + tgpu.ImageUsage.copyDst +
                tgpu.ImageUsage.storage + tgpu.ImageUsage.colorAttachment +
                tgpu.ImageUsage.sampled,
        },
        sampler: {},
    });
}
let presentImage = createPresentImage(extent.width, extent.height);

const bindless = gpu.bindless({
    maxRwBuffers: 1,
    maxSampledImages: 1,
    maxStorageImages: 1,
    maxSamplers: 1,
});

const pc = gpu.pushConstants(tgpu.PushConstants);
pc.particles = tgpu.bindlessAddRwBuffer(bindless, particleBuffer);
pc.output_image = tgpu.bindlessAddStorageImage(
    bindless,
    tgpu.viewImageView(presentImage),
    tgpu.ImageLayout.general
);
pc.present_texture = tgpu.bindlessAddSampledImage(
    bindless,
    tgpu.viewImageView(presentImage),
    tgpu.ImageLayout.fragment
);
pc.present_sampler = tgpu.bindlessAddSampler(bindless, tgpu.viewImageSampler(presentImage));
pc.window.width = extent.width;
pc.window.height = extent.height;

const shader = gpu.shaderSlang(SHADER_PATH);

const computePipeline = gpu.computePipeline({
    shader,
    entry: "computeMain",
    bindless,
    pushConstants: pc,
});
const clearPipeline = gpu.computePipeline({
    shader,
    entry: "computeClear",
    bindless,
    pushConstants: pc,
});
const renderPipeline = gpu.renderPipeline({
    shader,
    vertexEntry: "vertexMain",
    fragmentEntry: "fragmentMain",
    colorFormats: [colorFormat],
    bindless,
    pushConstants: pc,
});

const attachments = gpu.array(tgpu.ColorAttachment, 1);
const renderInfo = gpu.own(tgpu.renderDesc({ colorCount: 1, colors: attachments }));

let lastTime = glfwWindow.time();
let smoothedDt = 1.0 / 60.0;
let frameCount = 0;

while (glfwWindow.isOpen()) {
    glfwWindow.poll();

    const { width: frameWidth, height: frameHeight } = glfwWindow.framebufferSize();

    if (frameWidth <= 0 || frameHeight <= 0) {
        continue;
    }
    if (frameWidth !== extent.width || frameHeight !== extent.height) {
        extent.width = frameWidth;
        extent.height = frameHeight;
        gpu.swapchainSetExtent(swapchain, extent);
        gpu.swapchainRecreate({ swapchain });
    }

    const now = glfwWindow.time();
    let measured = now - lastTime;
    lastTime = now;
    if (measured < 1.0 / 240.0) {
        measured = 1.0 / 240.0;
    }
    if (measured > 1.0 / 15.0) {
        measured = 1.0 / 15.0;
    }
    smoothedDt = smoothedDt * 0.9 + measured * 0.1;

    const { frame, extent: current, resized } = gpu.acquireFrame(swapchain);
    if (resized) {
        extent = current;
        presentImage = createPresentImage(extent.width, extent.height);

        gpu.bindlessUpdateStorageImage({
            bindless,
            handle: pc.output_image,
            image: presentImage,
            layout: tgpu.ImageLayout.general,
        });
        gpu.bindlessUpdateSampledImage({
            bindless,
            handle: pc.present_texture,
            image: presentImage,
            layout: tgpu.ImageLayout.fragment,
        });
        gpu.bindlessUpdateSampler({
            bindless,
            handle: pc.present_sampler,
            image: presentImage,
        });
        pc.window.width = extent.width;
        pc.window.height = extent.height;
    }

    pc.mouse.x = mouse.x;
    pc.mouse.y = mouse.y;
    pc.dt = smoothedDt * 6.0;

    const recorder = gpu.recorderBegin({ queue });

    gpu.recorderTransitionImage({
        recorder,
        image: presentImage,
        layout: tgpu.ImageLayout.compute,
    });

    gpu.recorderBindCompute({
        recorder,
        pipeline: computePipeline,
        bindless,
        pushConstants: pc,
    });
    gpu.recorderDispatch({
        recorder,
        x: ceil_div(PARTICLE_COUNT, 256),
        y: 1,
        z: 1,
    });

    gpu.recorderBindCompute({
        recorder,
        pipeline: clearPipeline,
        bindless,
        pushConstants: pc,
    });
    gpu.recorderDispatch({
        recorder,
        x: ceil_div(extent.width, 16),
        y: ceil_div(extent.height, 16),
        z: 1,
    });

    gpu.recorderTransitionImage({
        recorder,
        image: presentImage,
        layout: tgpu.ImageLayout.fragment,
    });

    gpu.recorderTransitionFrame({
        recorder,
        swapchain,
        frame,
        layout: tgpu.ImageLayout.color,
    });

    const color = sys.raw.fabsf(sys.raw.sinf(frameCount / 120.0));
    gpu.writeColorAttachment({
        attachments,
        index: 0,
        view: gpu.swapchainView({ swapchain, frame }),
        layout: tgpu.ImageLayout.color,
        load: tgpu.LoadOp.clear,
        store: tgpu.StoreOp.store,
        clearColor: [0.0, color, 1.0 - color, 1.0],
    });

    renderInfo.width = extent.width;
    renderInfo.height = extent.height;

    gpu.render({
        recorder,
        desc: renderInfo,
        pipeline: renderPipeline,
        bindless,
        pushConstants: pc,
    }, (recorder) => {
        gpu.recorderDraw({
            recorder,
            firstVertex: 0,
            vertexCount: 6,
            firstInstance: 0,
            instanceCount: 1,
        });
    });

    gpu.recorderTransitionFrame({
        recorder,
        swapchain,
        frame,
        layout: tgpu.ImageLayout.present,
    });

    const command = gpu.recorderFinish({ recorder });
    gpu.swapchainEndFrame({ queue, swapchain, frame, command });

    frameCount = frameCount + 1;
}

gpu.dispose();

glfwWindow.dispose();

console.log("done");
