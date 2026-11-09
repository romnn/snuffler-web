//! Generates the `Operation` enum and the scheduling metadata of every operation.
//!
//! Operations come from two sources:
//!
//! - Each `BridgeService` rpc, whose metadata is its required `activity` method option.
//! - The executor-owned [`LOCAL_OPERATIONS`], which never cross the worker.

use color_eyre::eyre::{self, OptionExt as _, bail, ensure, eyre};
use proc_macro2::TokenStream;
use prost_reflect::{
    DescriptorPool, DynamicMessage, ExtensionDescriptor, Kind, MethodDescriptor,
    ReflectMessage as _,
};
use quote::{ToTokens, format_ident, quote};

/// Fully qualified name of the bridge service.
const SERVICE: &str = "snuffler.bridge.v1.BridgeService";

/// Fully qualified name of the `activity` method-option extension.
const ACTIVITY_OPTION: &str = "snuffler.bridge.v1.activity";

/// Executor lane an operation is scheduled on.
#[derive(Clone, Copy)]
enum Lane {
    Job,
    View,
    Local,
}

impl Lane {
    /// Maps an `ActivityLane` value; `Local` belongs to local operations only.
    fn from_proto(value: &str) -> Option<Self> {
        match value {
            "ACTIVITY_LANE_JOB" => Some(Self::Job),
            "ACTIVITY_LANE_VIEW" => Some(Self::View),
            _ => None,
        }
    }
}

impl ToTokens for Lane {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend(match self {
            Self::Job => quote!(Lane::Job),
            Self::View => quote!(Lane::View),
            Self::Local => quote!(Lane::Local),
        });
    }
}

/// How a new submission interacts with work already queued on its lane.
#[derive(Clone, Copy)]
enum Policy {
    Queue,
    Dedup,
    LatestWins,
}

impl Policy {
    fn from_proto(value: &str) -> Option<Self> {
        match value {
            "ACTIVITY_POLICY_QUEUE" => Some(Self::Queue),
            "ACTIVITY_POLICY_DEDUP" => Some(Self::Dedup),
            "ACTIVITY_POLICY_LATEST_WINS" => Some(Self::LatestWins),
            _ => None,
        }
    }
}

impl ToTokens for Policy {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend(match self {
            Self::Queue => quote!(Policy::Queue),
            Self::Dedup => quote!(Policy::Dedup),
            Self::LatestWins => quote!(Policy::LatestWins),
        });
    }
}

/// Whether a running operation can be cancelled.
#[derive(Clone, Copy)]
enum Cancellation {
    NotSupported,
    Supported,
}

impl Cancellation {
    fn from_proto(value: &str) -> Option<Self> {
        match value {
            "CANCELLATION_SUPPORT_NOT_SUPPORTED" => Some(Self::NotSupported),
            "CANCELLATION_SUPPORT_SUPPORTED" => Some(Self::Supported),
            _ => None,
        }
    }
}

impl ToTokens for Cancellation {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend(match self {
            Self::NotSupported => quote!(CancellationCapability::NotSupported),
            Self::Supported => quote!(CancellationCapability::Supported),
        });
    }
}

/// One `Operation` variant together with the `ActivityMeta` it maps to.
pub struct OperationActivity {
    name: String,
    label: String,
    lane: Lane,
    policy: Policy,
    secondary_feedback: bool,
    cancellation: Cancellation,
    supersedes: bool,
    may_update_markers: bool,
    exclusive_across_lanes: bool,
}

/// Metadata of an operation the executor runs without an rpc.
struct LocalOperation {
    name: &'static str,
    label: &'static str,
    lane: Lane,
    policy: Policy,
    secondary_feedback: bool,
    cancellation: Cancellation,
    may_update_markers: bool,
    exclusive_across_lanes: bool,
}

impl From<&LocalOperation> for OperationActivity {
    fn from(operation: &LocalOperation) -> Self {
        Self {
            name: operation.name.to_owned(),
            label: operation.label.to_owned(),
            lane: operation.lane,
            policy: operation.policy,
            secondary_feedback: operation.secondary_feedback,
            cancellation: operation.cancellation,
            supersedes: false,
            may_update_markers: operation.may_update_markers,
            exclusive_across_lanes: operation.exclusive_across_lanes,
        }
    }
}

/// Executor-owned operations with no rpc.
///
/// They never cross the worker, so their metadata is declared here rather than in the
/// `.proto`.
/// The enum listing and the match arms are both generated from this one table, so the two
/// cannot drift.
const LOCAL_OPERATIONS: [LocalOperation; 8] = [
    LocalOperation {
        name: "ReloadSnufflings",
        label: "Reloading snufflings",
        lane: Lane::Job,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::NotSupported,
        may_update_markers: false,
        exclusive_across_lanes: true,
    },
    LocalOperation {
        name: "OpenSelectedFiles",
        label: "Opening files",
        lane: Lane::Job,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::Supported,
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperation {
        name: "OpenDirectory",
        label: "Opening directory",
        lane: Lane::Job,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::Supported,
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperation {
        name: "ReloadOpenFiles",
        label: "Reloading files",
        lane: Lane::Job,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::Supported,
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperation {
        name: "PollWatchedFiles",
        label: "Checking watched files",
        lane: Lane::Job,
        policy: Policy::Dedup,
        secondary_feedback: true,
        cancellation: Cancellation::Supported,
        may_update_markers: true,
        exclusive_across_lanes: false,
    },
    LocalOperation {
        name: "ExportSvg",
        label: "Exporting SVG",
        lane: Lane::Local,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::NotSupported,
        may_update_markers: false,
        exclusive_across_lanes: false,
    },
    LocalOperation {
        name: "ExportPng",
        label: "Exporting PNG",
        lane: Lane::Local,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::NotSupported,
        may_update_markers: false,
        exclusive_across_lanes: false,
    },
    LocalOperation {
        name: "SerializeMarkers",
        label: "Serializing markers",
        lane: Lane::Local,
        policy: Policy::Queue,
        secondary_feedback: false,
        cancellation: Cancellation::NotSupported,
        may_update_markers: false,
        exclusive_across_lanes: false,
    },
];

/// Collects every operation: the rpcs in schema order, then the local operations.
pub fn operations(descriptors: &DescriptorPool) -> eyre::Result<Vec<OperationActivity>> {
    let service = descriptors
        .get_service_by_name(SERVICE)
        .ok_or_eyre("BridgeService descriptor is missing")?;
    let activity_option = descriptors
        .get_extension_by_name(ACTIVITY_OPTION)
        .ok_or_eyre("activity method-option descriptor is missing")?;

    let mut operations = service
        .methods()
        .map(|method| rpc_operation(&method, &activity_option))
        .collect::<eyre::Result<Vec<_>>>()?;
    operations.extend(LOCAL_OPERATIONS.iter().map(OperationActivity::from));
    Ok(operations)
}

/// Reads and validates the `activity` option of one rpc.
fn rpc_operation(
    method: &MethodDescriptor,
    activity_option: &ExtensionDescriptor,
) -> eyre::Result<OperationActivity> {
    let name = method.name();
    let options = method.options();
    ensure!(
        options.has_extension(activity_option),
        "BridgeService.{name}: missing required activity option"
    );
    let value = options.get_extension(activity_option);
    let message = value
        .as_message()
        .ok_or_else(|| eyre!("BridgeService.{name}: activity option is not a message"))?;
    let activity = ActivityOption {
        method: name,
        message,
    };

    let label = activity.string("label")?;
    ensure!(
        !label.is_empty(),
        "BridgeService.{name}: activity label is empty"
    );

    // Only the Python dispatcher consumes session access.
    // Validate it here too so no generated endpoint accepts an unspecified contract.
    activity.enum_value("session_access", |_| Some(()))?;

    Ok(OperationActivity {
        name: name.to_owned(),
        label,
        lane: activity.enum_value("lane", Lane::from_proto)?,
        policy: activity.enum_value("policy", Policy::from_proto)?,
        secondary_feedback: activity.bool("secondary_feedback")?,
        cancellation: activity.enum_value("cancellation", Cancellation::from_proto)?,
        supersedes: activity.bool("supersedes")?,
        may_update_markers: activity.bool("may_update_markers")?,
        // Cross-lane exclusion is a local-operation concept with no field in the schema.
        exclusive_across_lanes: false,
    })
}

/// Typed field access to one rpc's `Activity` message, with errors naming the rpc.
struct ActivityOption<'a> {
    method: &'a str,
    message: &'a DynamicMessage,
}

impl ActivityOption<'_> {
    fn string(&self, field: &str) -> eyre::Result<String> {
        self.message
            .get_field_by_name(field)
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| eyre!("Activity.{field} is not a string"))
    }

    fn bool(&self, field: &str) -> eyre::Result<bool> {
        self.message
            .get_field_by_name(field)
            .and_then(|value| value.as_bool())
            .ok_or_else(|| eyre!("Activity.{field} is not a bool"))
    }

    /// Resolves an enum field to its value name and maps it with `parse`.
    ///
    /// An `*_UNSPECIFIED` value is always rejected, since every rpc must choose explicitly.
    fn enum_value<T>(&self, field: &str, parse: impl FnOnce(&str) -> Option<T>) -> eyre::Result<T> {
        let method = self.method;
        let descriptor = self
            .message
            .descriptor()
            .get_field_by_name(field)
            .ok_or_else(|| eyre!("Activity.{field} descriptor is missing"))?;
        let Kind::Enum(enum_type) = descriptor.kind() else {
            bail!("Activity.{field} is not an enum");
        };

        let number = self
            .message
            .get_field(&descriptor)
            .as_enum_number()
            .ok_or_else(|| eyre!("Activity.{field} is not an enum value"))?;
        let value = enum_type.get_value(number).ok_or_else(|| {
            eyre!(
                "BridgeService.{method}: activity {field} has unknown {} value {number}",
                enum_type.name()
            )
        })?;

        let value_name = value.name();
        ensure!(
            !value_name.ends_with("_UNSPECIFIED"),
            "BridgeService.{method}: activity {field} is UNSPECIFIED"
        );
        parse(value_name).ok_or_else(|| {
            eyre!("BridgeService.{method}: activity {field} {value_name} is not valid for an rpc")
        })
    }
}

/// Renders the scheduling types, the `Operation` enum and its `activity()` table.
pub fn render(operations: &[OperationActivity]) -> TokenStream {
    let variants = operations
        .iter()
        .map(|operation| format_ident!("{}", operation.name));
    let arms = operations.iter().map(render_match_arm);

    quote! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Lane {
            Job,
            View,
            Local,
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Policy {
            Queue,
            Dedup,
            LatestWins,
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum CancellationCapability {
            NotSupported,
            Supported,
        }

        /// Scheduling metadata of one [`Operation`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct ActivityMeta {
            pub label: &'static str,
            pub lane: Lane,
            pub policy: Policy,
            pub secondary_feedback: bool,
            pub cancellation: CancellationCapability,
            pub supersedes: bool,
            pub may_update_markers: bool,
            pub exclusive_across_lanes: bool,
        }

        /// Every operation the executor schedules: one per `BridgeService` rpc, then the
        /// local operations that never cross the worker.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Operation {
            #(#variants,)*
        }

        impl Operation {
            pub const fn activity(self) -> ActivityMeta {
                match self {
                    #(#arms)*
                }
            }
        }
    }
}

fn render_match_arm(operation: &OperationActivity) -> TokenStream {
    let OperationActivity {
        name,
        label,
        lane,
        policy,
        secondary_feedback,
        cancellation,
        supersedes,
        may_update_markers,
        exclusive_across_lanes,
    } = operation;
    let variant = format_ident!("{name}");

    quote! {
        Self::#variant => ActivityMeta {
            label: #label,
            lane: #lane,
            policy: #policy,
            secondary_feedback: #secondary_feedback,
            cancellation: #cancellation,
            supersedes: #supersedes,
            may_update_markers: #may_update_markers,
            exclusive_across_lanes: #exclusive_across_lanes,
        },
    }
}
