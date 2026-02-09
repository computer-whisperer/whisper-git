use anyhow::{Context, Result};
use std::sync::Arc;
use vulkano::{
    command_buffer::allocator::StandardCommandBufferAllocator,
    device::{
        physical::PhysicalDeviceType, Device, DeviceCreateInfo, DeviceExtensions, Queue,
        QueueCreateInfo, QueueFlags,
    },
    instance::{Instance, InstanceCreateInfo},
    memory::allocator::StandardMemoryAllocator,
    swapchain::Surface,
    VulkanLibrary,
};
use winit::event_loop::EventLoop;

/// Core Vulkan context - created once at startup
#[allow(dead_code)]
pub struct VulkanContext {
    pub instance: Arc<Instance>,
    pub device: Arc<Device>,
    pub queue: Arc<Queue>,
    pub memory_allocator: Arc<StandardMemoryAllocator>,
    pub command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
}

#[allow(dead_code)]
impl VulkanContext {
    /// Create a new Vulkan context
    pub fn new(event_loop: &EventLoop<()>) -> Result<Self> {
        let library = VulkanLibrary::new().context("No Vulkan library found")?;

        let required_extensions = Surface::required_extensions(event_loop)
            .context("Failed to get required surface extensions")?;

        let instance = Instance::new(
            library,
            InstanceCreateInfo {
                enabled_extensions: required_extensions,
                ..Default::default()
            },
        )
        .context("Failed to create Vulkan instance")?;

        Self::with_instance(instance)
    }

    /// Create context with an existing instance (for when we need surface first)
    pub fn with_instance(_instance: Arc<Instance>) -> Result<Self> {
        // We need a temporary surface to check device compatibility
        // This will be called after we have a window
        // For now, just store instance - device creation deferred

        // This is a placeholder - real initialization happens in with_surface
        Err(anyhow::anyhow!("Use with_surface instead"))
    }

    /// Create context with a surface (needed for device selection)
    pub fn with_surface(instance: Arc<Instance>, surface: &Surface) -> Result<Self> {
        let device_extensions = DeviceExtensions {
            khr_swapchain: true,
            ..DeviceExtensions::empty()
        };

        let (physical_device, queue_family_index) = instance
            .enumerate_physical_devices()
            .context("Failed to enumerate physical devices")?
            .filter(|p| p.supported_extensions().contains(&device_extensions))
            .filter_map(|p| {
                p.queue_family_properties()
                    .iter()
                    .enumerate()
                    .position(|(i, q)| {
                        q.queue_flags.contains(QueueFlags::GRAPHICS)
                            && p.surface_support(i as u32, surface).unwrap_or(false)
                    })
                    .map(|i| (p, i as u32))
            })
            .min_by_key(|(p, _)| match p.properties().device_type {
                PhysicalDeviceType::DiscreteGpu => 0,
                PhysicalDeviceType::IntegratedGpu => 1,
                PhysicalDeviceType::VirtualGpu => 2,
                PhysicalDeviceType::Cpu => 3,
                PhysicalDeviceType::Other => 4,
                _ => 5,
            })
            .context("No suitable GPU found")?;

        println!(
            "Using device: {} ({:?})",
            physical_device.properties().device_name,
            physical_device.properties().device_type
        );

        let (device, mut queues) = Device::new(
            physical_device,
            DeviceCreateInfo {
                enabled_extensions: device_extensions,
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .context("Failed to create device")?;

        let queue = queues.next().context("No queue available")?;

        let memory_allocator = Arc::new(StandardMemoryAllocator::new_default(device.clone()));
        let command_buffer_allocator = Arc::new(StandardCommandBufferAllocator::new(
            device.clone(),
            Default::default(),
        ));

        Ok(Self {
            instance,
            device,
            queue,
            memory_allocator,
            command_buffer_allocator,
        })
    }
}
