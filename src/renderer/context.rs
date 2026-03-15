//! Vulkan device, queue, and allocator setup.
//!
//! VulkanContext owns the fundamental Vulkan objects created once at startup.

use anyhow::{Context, Result};
use std::sync::Arc;
use vulkano::{
    command_buffer::allocator::StandardCommandBufferAllocator,
    device::{
        Device, DeviceCreateInfo, DeviceExtensions, Queue, QueueCreateInfo, QueueFlags,
        physical::PhysicalDeviceType,
    },
    instance::Instance,
    memory::allocator::StandardMemoryAllocator,
    swapchain::Surface,
};

/// Core Vulkan context - created once at startup
pub struct VulkanContext {
    pub device: Arc<Device>,
    pub queue: Arc<Queue>,
    pub memory_allocator: Arc<StandardMemoryAllocator>,
    pub command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
}

impl VulkanContext {
    /// Create context with a surface (needed for device selection).
    ///
    /// GPU preference can be overridden with `WHISPER_GPU`:
    /// - `integrated` — prefer integrated GPU (lower power, quieter)
    /// - `discrete` — prefer discrete GPU (default)
    /// - a substring of a device name — select that specific device
    pub fn with_surface(instance: Arc<Instance>, surface: &Surface) -> Result<Self> {
        let device_extensions = DeviceExtensions {
            khr_swapchain: true,
            ..DeviceExtensions::empty()
        };

        let gpu_pref = std::env::var("WHISPER_GPU").ok();

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
            .min_by_key(|(p, _)| {
                let dev_type = p.properties().device_type;
                let dev_name = p.properties().device_name.to_lowercase();
                match gpu_pref.as_deref() {
                    Some("integrated") => match dev_type {
                        PhysicalDeviceType::IntegratedGpu => 0,
                        PhysicalDeviceType::DiscreteGpu => 1,
                        PhysicalDeviceType::VirtualGpu => 2,
                        PhysicalDeviceType::Cpu => 3,
                        PhysicalDeviceType::Other => 4,
                        _ => 5,
                    },
                    Some("discrete") | None => match dev_type {
                        PhysicalDeviceType::DiscreteGpu => 0,
                        PhysicalDeviceType::IntegratedGpu => 1,
                        PhysicalDeviceType::VirtualGpu => 2,
                        PhysicalDeviceType::Cpu => 3,
                        PhysicalDeviceType::Other => 4,
                        _ => 5,
                    },
                    Some(name) => {
                        if dev_name.contains(&name.to_lowercase()) {
                            0
                        } else {
                            1
                        }
                    }
                }
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
            device,
            queue,
            memory_allocator,
            command_buffer_allocator,
        })
    }
}
