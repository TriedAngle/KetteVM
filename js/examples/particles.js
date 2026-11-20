// Particles example
// Run from the repository root:
//   cargo run -q -p vm -- js/examples/particles.js

KetteTools.evalFile("kette/vendor/sys.ktt");
KetteTools.evalFile("kette/vendor/tgpu.ktt");
KetteTools.evalFile("kette/vendor/glfw.ktt");

const tgpu = globalThis.tgpu;
const glfw = globalThis.glfw;
const sys = globalThis.sys;
const Ptr = FFI.pointer(FFI.void);

const PARTICLE_COUNT = 10000;
const FLOATS_PER_PARTICLE = 8;
const PARTICLE_BYTES = PARTICLE_COUNT * FLOATS_PER_PARTICLE * 4;
const SHADER_PATH = "kette/examples/tgpu/shader.slang";

function ceil_div(value, divisor) {
    const adjusted = value + divisor - 1;
    return (adjusted - (adjusted % divisor)) / divisor;
}

const glfwWindow = glfw.create("tgpu particles (JS)", 800, 600);

const framebufferSize = FFI.array(FFI.i32, 2).new();
glfw.framebufferSize(glfwWindow, framebufferSize.add(0), framebufferSize.add(1));
const startWidth = framebufferSize.get(0);
const startHeight = framebufferSize.get(1);
if (startWidth <= 0 || startHeight <= 0) {
    console.log("window has no framebuffer");
    sys.raw.exit(1);
}

// Mouse position in framebuffer coordinates, written by the GLFW callback.
const mouse = FFI.array(FFI.f32, 2).new();
const windowSize = FFI.array(FFI.i32, 2).new();
const cursorFramebuffer = FFI.array(FFI.i32, 2).new();

const onCursor = FFI.callback([Ptr, FFI.f64, FFI.f64], FFI.void, (windowHandle, x, y) => {
    glfw.windowSize(windowHandle, windowSize.add(0), windowSize.add(1));
    glfw.framebufferSize(windowHandle, cursorFramebuffer.add(0), cursorFramebuffer.add(1));
    const windowWidth = windowSize.get(0);
    const windowHeight = windowSize.get(1);
    const frameWidth = cursorFramebuffer.get(0);
    const frameHeight = cursorFramebuffer.get(1);

    let scaleX = 1.0;
    let scaleY = 1.0;
    if (windowWidth > 0) {
        scaleX = frameWidth / windowWidth;
    }
    if (windowHeight > 0) {
        scaleY = frameHeight / windowHeight;
    }
    mouse.set(0, x * scaleX);
    mouse.set(1, frameHeight - y * scaleY);
});
glfw.setCursorPosCallback(glfwWindow, onCursor);

const onKey = FFI.callback([Ptr, FFI.i32, FFI.i32, FFI.i32, FFI.i32], FFI.void,
    (windowHandle, key, scancode, action, mods) => {
        // GLFW_PRESS = 1, GLFW_KEY_ESCAPE = 256
        if (action === 1 && key === 256) {
            glfw.setShouldClose(windowHandle, 1);
        }
    });
glfw.setKeyCallback(glfwWindow, onKey);

const instance = tgpu.instanceCreate(null);
tgpu.check();

const queues = FFI.array(Ptr, 1).new();
const device = tgpu.deviceCreate(instance, null, queues, 1);
tgpu.check();
const queue = queues.get(0);

const swapchainDesc = tgpu.SwapchainDesc.new();
swapchainDesc.struct_size = tgpu.SwapchainDesc.size();
swapchainDesc.window = glfw.tgpuWindow(glfwWindow);
swapchainDesc.width = startWidth;
swapchainDesc.height = startHeight;
swapchainDesc.image_count = 3;
swapchainDesc.present_mode = tgpu.PresentMode.FIFO;
const swapchain = tgpu.swapchainCreate(device, swapchainDesc);
tgpu.check();

const extent = tgpu.Extent2D.new();
tgpu.swapchainExtent(swapchain, extent);
tgpu.check();
const colorFormat = tgpu.swapchainFormat(swapchain);

const particleData = FFI.array(FFI.f32, PARTICLE_COUNT * FLOATS_PER_PARTICLE).new();
const randomUnit = () => sys.raw.rand() / 2147483647.0;
sys.raw.srand(1);
let particleIndex = 0;
while (particleIndex < PARTICLE_COUNT) {
    const base = particleIndex * FLOATS_PER_PARTICLE;
    particleData.set(base + 0, randomUnit() * extent.width);
    particleData.set(base + 1, randomUnit() * extent.height);
    particleData.set(base + 2, (randomUnit() * 2.0 - 1.0) * 30.0);
    particleData.set(base + 3, (randomUnit() * 2.0 - 1.0) * 30.0);
    particleData.set(base + 4, 0.5 + randomUnit() * 0.5);
    particleData.set(base + 5, 0.5 + randomUnit() * 0.5);
    particleData.set(base + 6, 0.5 + randomUnit() * 0.5);
    particleData.set(base + 7, 1.0);
    particleIndex = particleIndex + 1;
}

const bufferDesc = tgpu.BufferDesc.new();
bufferDesc.struct_size = tgpu.BufferDesc.size();
bufferDesc.size = PARTICLE_BYTES;
bufferDesc.usage = tgpu.BufferUsage.STORAGE + tgpu.BufferUsage.COPY_DST;
bufferDesc.memory = tgpu.MemoryPreset.DYNAMIC;
bufferDesc.host_access = tgpu.HostAccess.READ_WRITE_RANDOM;
const particleBuffer = tgpu.bufferCreate(
    device,
    bufferDesc,
    particleData.cast(FFI.u8),
    PARTICLE_BYTES
);

const samplerDesc = tgpu.SamplerDesc.new();
samplerDesc.struct_size = tgpu.SamplerDesc.size();
samplerDesc.mag = tgpu.Filter.LINEAR;
samplerDesc.min = tgpu.Filter.LINEAR;
samplerDesc.mipmap = tgpu.SamplerMipmapMode.LINEAR;
samplerDesc.address_u = tgpu.SamplerAddressMode.CLAMP_TO_EDGE;
samplerDesc.address_v = tgpu.SamplerAddressMode.CLAMP_TO_EDGE;
samplerDesc.address_w = tgpu.SamplerAddressMode.CLAMP_TO_EDGE;

const imageDesc = tgpu.ImageDesc.new();
imageDesc.struct_size = tgpu.ImageDesc.size();
imageDesc.format = colorFormat;
imageDesc.width = extent.width;
imageDesc.height = extent.height;
imageDesc.depth = 1;
imageDesc.mip_levels = 1;
imageDesc.array_layers = 1;
imageDesc.usage = tgpu.ImageUsage.COPY_SRC + tgpu.ImageUsage.COPY_DST +
    tgpu.ImageUsage.STORAGE + tgpu.ImageUsage.COLOR_ATTACHMENT + tgpu.ImageUsage.SAMPLED;
imageDesc.memory = tgpu.MemoryPreset.GPU_ONLY;
imageDesc.host_access = tgpu.HostAccess.NONE;
imageDesc.initial_layout = tgpu.ImageLayout.UNDEFINED;

const viewImageDesc = tgpu.ViewImageDesc.new();
viewImageDesc.struct_size = tgpu.ViewImageDesc.size();
viewImageDesc.image = imageDesc;
viewImageDesc.sampler = samplerDesc;
let presentImage = tgpu.viewImageCreate(device, viewImageDesc);

const bindlessDesc = tgpu.BindlessDesc.new();
bindlessDesc.struct_size = tgpu.BindlessDesc.size();
bindlessDesc.max_rw_buffers = 1;
bindlessDesc.max_sampled_images = 1;
bindlessDesc.max_storage_images = 1;
bindlessDesc.max_samplers = 1;
const bindless = tgpu.bindlessCreate(device, bindlessDesc);
tgpu.check();

const pc = tgpu.PushConstants.new();
pc.particles = tgpu.bindlessAddRwBuffer(bindless, particleBuffer);
pc.output_image = tgpu.bindlessAddStorageImage(
    bindless,
    tgpu.viewImageView(presentImage),
    tgpu.ImageLayout.GENERAL
);
pc.present_texture = tgpu.bindlessAddSampledImage(
    bindless,
    tgpu.viewImageView(presentImage),
    tgpu.ImageLayout.FRAGMENT
);
pc.present_sampler = tgpu.bindlessAddSampler(bindless, tgpu.viewImageSampler(presentImage));
tgpu.check();
pc.window.set(0, extent.width);
pc.window.set(1, extent.height);

const shader = tgpu.loadSlang(device, SHADER_PATH);
tgpu.check();

const entryComputeMain = FFI.cstring("computeMain");
const entryComputeClear = FFI.cstring("computeClear");
const entryVertexMain = FFI.cstring("vertexMain");
const entryFragmentMain = FFI.cstring("fragmentMain");

const computeDesc = tgpu.ComputePipelineDesc.new();
computeDesc.struct_size = tgpu.ComputePipelineDesc.size();
computeDesc.shader = shader;
computeDesc.entry = entryComputeMain;
computeDesc.bindless = bindless;
computeDesc.push_constant_size = tgpu.PushConstants.size();
const computePipeline = tgpu.computePipelineCreate(device, computeDesc);

const clearDesc = tgpu.ComputePipelineDesc.new();
clearDesc.struct_size = tgpu.ComputePipelineDesc.size();
clearDesc.shader = shader;
clearDesc.entry = entryComputeClear;
clearDesc.bindless = bindless;
clearDesc.push_constant_size = tgpu.PushConstants.size();
const clearPipeline = tgpu.computePipelineCreate(device, clearDesc);

const colorFormats = FFI.array(FFI.i32, 1).new();
colorFormats.set(0, colorFormat);

const renderDesc = tgpu.RenderPipelineDesc.new();
renderDesc.struct_size = tgpu.RenderPipelineDesc.size();
renderDesc.vertex_shader = shader;
renderDesc.vertex_entry = entryVertexMain;
renderDesc.fragment_shader = shader;
renderDesc.fragment_entry = entryFragmentMain;
renderDesc.color_formats = colorFormats;
renderDesc.color_format_count = 1;
renderDesc.depth_format = tgpu.Format.UNDEFINED;
renderDesc.depth_test = false;
renderDesc.depth_write = false;
renderDesc.depth_compare = 0;
renderDesc.bindless = bindless;
renderDesc.push_constant_size = tgpu.PushConstants.size();
renderDesc.cull = tgpu.CullMode.BACK;
renderDesc.front_face = tgpu.FrontFace.COUNTER_CLOCKWISE;
renderDesc.topology = tgpu.Topology.TRIANGLE_LIST;
renderDesc.blend = tgpu.Blend.DISABLED;
const renderPipeline = tgpu.renderPipelineCreate(device, renderDesc);
tgpu.check();

const frame = tgpu.Frame.new();
const currentExtent = tgpu.Extent2D.new();
const attachment = tgpu.ColorAttachment.new();
const attachments = FFI.array(tgpu.ColorAttachment, 1).new();
const renderInfo = tgpu.RenderDesc.new();
renderInfo.struct_size = tgpu.RenderDesc.size();
renderInfo.color_count = 1;
renderInfo.colors = attachments;

let lastTime = glfw.time();
let smoothedDt = 1.0 / 60.0;
let frameCount = 0;

while (glfw.shouldClose(glfwWindow) === 0) {
    glfw.pollEvents();

    glfw.framebufferSize(glfwWindow, framebufferSize.add(0), framebufferSize.add(1));
    const frameWidth = framebufferSize.get(0);
    const frameHeight = framebufferSize.get(1);

    if (frameWidth > 0 && frameHeight > 0) {
        if (frameWidth !== extent.width || frameHeight !== extent.height) {
            extent.width = frameWidth;
            extent.height = frameHeight;
            tgpu.swapchainSetExtent(swapchain, extent);
            tgpu.swapchainRecreate(swapchain);
            tgpu.check();
        }

        const now = glfw.time();
        let measured = now - lastTime;
        lastTime = now;
        if (measured < 1.0 / 240.0) {
            measured = 1.0 / 240.0;
        }
        if (measured > 1.0 / 15.0) {
            measured = 1.0 / 15.0;
        }
        smoothedDt = smoothedDt * 0.9 + measured * 0.1;

        tgpu.swapchainBeginFrame(queue, swapchain, frame);
        tgpu.check();

        tgpu.swapchainExtent(swapchain, currentExtent);
        tgpu.check();
        if (currentExtent.width !== extent.width || currentExtent.height !== extent.height) {
            extent.width = currentExtent.width;
            extent.height = currentExtent.height;
            tgpu.deviceWaitIdle(device);
            tgpu.viewImageDestroy(presentImage);

            viewImageDesc.image.width = extent.width;
            viewImageDesc.image.height = extent.height;
            presentImage = tgpu.viewImageCreate(device, viewImageDesc);
            tgpu.check();

            tgpu.bindlessUpdateStorageImage(
                bindless,
                pc.output_image,
                tgpu.viewImageView(presentImage),
                tgpu.ImageLayout.GENERAL
            );
            tgpu.bindlessUpdateSampledImage(
                bindless,
                pc.present_texture,
                tgpu.viewImageView(presentImage),
                tgpu.ImageLayout.FRAGMENT
            );
            tgpu.bindlessUpdateSampler(
                bindless,
                pc.present_sampler,
                tgpu.viewImageSampler(presentImage)
            );
            pc.window.set(0, extent.width);
            pc.window.set(1, extent.height);
        }

        pc.mouse.set(0, mouse.get(0));
        pc.mouse.set(1, mouse.get(1));
        pc.dt = smoothedDt * 6.0;

        const recorder = tgpu.recorderBegin(queue);
        tgpu.check();

        tgpu.recorderTransitionImage(
            recorder,
            tgpu.viewImageImage(presentImage),
            tgpu.ImageLayout.COMPUTE
        );

        tgpu.recorderBindCompute(recorder, computePipeline, bindless, pc, tgpu.PushConstants.size());
        tgpu.recorderDispatch(recorder, ceil_div(PARTICLE_COUNT, 256), 1, 1);

        tgpu.recorderBindCompute(recorder, clearPipeline, bindless, pc, tgpu.PushConstants.size());
        tgpu.recorderDispatch(
            recorder,
            ceil_div(extent.width, 16),
            ceil_div(extent.height, 16),
            1
        );

        tgpu.recorderTransitionImage(
            recorder,
            tgpu.viewImageImage(presentImage),
            tgpu.ImageLayout.FRAGMENT
        );

        tgpu.recorderTransitionFrame(
            recorder,
            swapchain,
            frame,
            tgpu.ImageLayout.COLOR
        );

        const color = sys.raw.fabsf(sys.raw.sinf(frameCount / 120.0));
        attachment.view = tgpu.swapchainView(swapchain, frame);
        attachment.layout = tgpu.ImageLayout.COLOR;
        attachment.load = tgpu.LoadOp.CLEAR;
        attachment.store = tgpu.StoreOp.STORE;
        attachment.clear_color.set(0, 0.0);
        attachment.clear_color.set(1, color);
        attachment.clear_color.set(2, 1.0 - color);
        attachment.clear_color.set(3, 1.0);
        attachments.set(0, attachment);

        renderInfo.width = extent.width;
        renderInfo.height = extent.height;

        tgpu.recorderBindRender(recorder, renderPipeline, bindless, pc, tgpu.PushConstants.size());
        tgpu.recorderBeginRender(recorder, renderInfo);
        tgpu.recorderDraw(recorder, 0, 6, 0, 1);
        tgpu.recorderEndRender(recorder);

        tgpu.recorderTransitionFrame(
            recorder,
            swapchain,
            frame,
            tgpu.ImageLayout.PRESENT
        );

        tgpu.check();

        const command = tgpu.recorderFinish(recorder);
        const commands = FFI.array(Ptr, 1).new();
        commands.set(0, command);

        tgpu.swapchainEndFrame(queue, swapchain, frame, commands, 1);
        tgpu.commandBufferDestroy(command);
        commands.free();
        tgpu.check();

        frameCount = frameCount + 1;
    }
}

tgpu.deviceWaitIdle(device);

tgpu.renderPipelineDestroy(renderPipeline);
tgpu.computePipelineDestroy(clearPipeline);
tgpu.computePipelineDestroy(computePipeline);
tgpu.shaderDestroy(shader);
tgpu.viewImageDestroy(presentImage);
tgpu.bindlessDestroy(bindless);
tgpu.bufferDestroy(particleBuffer);
tgpu.swapchainDestroy(swapchain);
tgpu.queueDestroy(queue);
tgpu.deviceDestroy(device);
tgpu.instanceDestroy(instance);

particleData.free();
framebufferSize.free();
mouse.free();
windowSize.free();
cursorFramebuffer.free();
queues.free();
colorFormats.free();
attachments.free();
swapchainDesc.free();
extent.free();
bufferDesc.free();
samplerDesc.free();
imageDesc.free();
viewImageDesc.free();
bindlessDesc.free();
pc.free();
computeDesc.free();
clearDesc.free();
renderDesc.free();
frame.free();
currentExtent.free();
attachment.free();
renderInfo.free();
entryComputeMain.free();
entryComputeClear.free();
entryVertexMain.free();
entryFragmentMain.free();

glfw.destroyWindow(glfwWindow);
glfw.terminate();

console.log("done");
