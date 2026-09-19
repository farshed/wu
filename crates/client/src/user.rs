use super::Client;
use gpui::{Context, SharedString, SharedUri};
use postage::watch;
use std::sync::Arc;

pub type LegacyUserId = u64;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct ProjectId(pub u64);

impl ProjectId {
    pub fn to_proto(self) -> u64 {
        self.0
    }
}

#[derive(Default, Debug)]
pub struct User {
    pub legacy_id: LegacyUserId,
    pub username: SharedString,
    pub avatar_uri: SharedUri,
    pub name: Option<String>,
}

impl PartialOrd for User {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for User {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.username.cmp(&other.username)
    }
}

impl PartialEq for User {
    fn eq(&self, other: &Self) -> bool {
        self.legacy_id == other.legacy_id && self.username == other.username
    }
}

impl Eq for User {}

pub struct UserStore {
    current_user: watch::Receiver<Option<Arc<User>>>,
    _current_user_tx: watch::Sender<Option<Arc<User>>>,
}

impl UserStore {
    pub fn new(_client: Arc<Client>, _cx: &Context<Self>) -> Self {
        let (current_user_tx, current_user_rx) = watch::channel();
        Self {
            current_user: current_user_rx,
            _current_user_tx: current_user_tx,
        }
    }

    pub fn current_user(&self) -> Option<Arc<User>> {
        self.current_user.borrow().clone()
    }

<<<<<<< a51db33c1c61b6350dc96f2b0044877dd737c03b
=======
    pub fn current_organization(&self) -> Option<Arc<Organization>> {
        self.current_organization.clone()
    }

    pub fn set_current_organization(
        &mut self,
        organization: Arc<Organization>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let is_same_organization = self
            .current_organization
            .as_ref()
            .is_some_and(|current| current.id == organization.id);

        if is_same_organization {
            return Task::ready(Ok(()));
        }

        let organization_id = organization.id.clone();
        self.current_organization.replace(organization);
        cx.emit(Event::OrganizationChanged);
        cx.notify();

        let Some(client) = self.client.upgrade() else {
            return Task::ready(Ok(()));
        };
        let Some(system_id) = client.telemetry().system_id().map(|id| id.to_string()) else {
            // Without a system ID we have no addressable target row on the
            // server, so the selection stays purely session-local.
            return Task::ready(Ok(()));
        };
        let cloud_client = client.cloud_client();

        cx.background_spawn(async move {
            let body = UpdateSystemSettingsBody {
                selected_organization_id: Some(organization_id),
            };
            cloud_client
                .update_system_settings(system_id, body)
                .await
                .context("failed to persist selected organization")?;
            Ok(())
        })
    }

    pub fn organizations(&self) -> &Vec<Arc<Organization>> {
        &self.organizations
    }

    pub fn plan_for_organization(&self, organization_id: &OrganizationId) -> Option<Plan> {
        self.plans_by_organization.get(organization_id).copied()
    }

    pub fn current_organization_configuration(&self) -> Option<&OrganizationConfiguration> {
        let current_organization = self.current_organization.as_ref()?;

        self.configuration_by_organization
            .get(&current_organization.id)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_current_organization_configuration_for_test(
        &mut self,
        organization: Arc<Organization>,
        configuration: OrganizationConfiguration,
        cx: &mut Context<Self>,
    ) {
        self.current_organization = Some(organization.clone());
        self.organizations = vec![organization.clone()];
        self.configuration_by_organization
            .insert(organization.id.clone(), configuration);
        cx.emit(Event::OrganizationChanged);
        cx.notify();
    }

    pub fn plan(&self) -> Option<Plan> {
        #[cfg(debug_assertions)]
        if let Ok(plan) = std::env::var("ZED_SIMULATE_PLAN").as_ref() {
            use cloud_api_client::Plan;

            return match plan.as_str() {
                "free" => Some(Plan::ZedFree),
                "trial" => Some(Plan::ZedProTrial),
                "pro" => Some(Plan::ZedPro),
                _ => {
                    panic!("ZED_SIMULATE_PLAN must be one of 'free', 'trial', or 'pro'");
                }
            };
        }

        if let Some(organization) = &self.current_organization {
            return self.plan_for_organization(&organization.id);
        }

        self.plan_info.as_ref().map(|info| info.plan())
    }

    pub fn subscription_period(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        self.plan_info
            .as_ref()
            .and_then(|plan| plan.subscription_period)
            .map(|subscription_period| {
                (
                    subscription_period.started_at.0,
                    subscription_period.ended_at.0,
                )
            })
    }

    pub fn trial_started_at(&self) -> Option<DateTime<Utc>> {
        self.plan_info
            .as_ref()
            .and_then(|plan| plan.trial_started_at)
            .map(|trial_started_at| trial_started_at.0)
    }

    /// Returns whether the user's account is too new to use the service.
    ///
    /// This only applies when operating under the user's personal organization,
    /// not a business organization.
    pub fn account_too_young(&self) -> bool {
        if let Some(org) = &self.current_organization {
            if !org.is_personal {
                return false;
            }
        }

        self.plan_info
            .as_ref()
            .map(|plan| plan.is_account_too_young)
            .unwrap_or_default()
    }

    /// Returns whether the current user has overdue invoices and usage should be blocked.
    pub fn has_overdue_invoices(&self) -> bool {
        self.plan_info
            .as_ref()
            .map(|plan| plan.has_overdue_invoices)
            .unwrap_or_default()
    }

    pub fn edit_prediction_usage(&self) -> Option<EditPredictionUsage> {
        self.edit_prediction_usage
    }

    pub fn update_edit_prediction_usage(
        &mut self,
        usage: EditPredictionUsage,
        cx: &mut Context<Self>,
    ) {
        self.edit_prediction_usage = Some(usage);
        cx.notify();
    }

    pub fn clear_organizations(&mut self) {
        self.organizations.clear();
        self.current_organization = None;
    }

    pub fn clear_plan_and_usage(&mut self) {
        self.plan_info = None;
        self.edit_prediction_usage = None;
    }

    fn update_authenticated_user(
        &mut self,
        response: GetAuthenticatedUserResponse,
        cx: &mut Context<Self>,
    ) {
        let staff = response.user.is_staff && !*feature_flags::ZED_DISABLE_STAFF;
        cx.update_flags(staff, response.feature_flags);
        if let Some(client) = self.client.upgrade() {
            client
                .telemetry
                .set_authenticated_user_info(Some(response.user.metrics_id.clone()), staff);
        }

        self.organizations = response.organizations.into_iter().map(Arc::new).collect();

        self.current_organization = response
            .default_organization_id
            .and_then(|default_organization_id| {
                self.organizations
                    .iter()
                    .find(|organization| organization.id == default_organization_id)
                    .cloned()
            })
            .or_else(|| self.organizations.first().cloned());
        self.plans_by_organization = response
            .plans_by_organization
            .into_iter()
            .map(|(organization_id, plan)| {
                let plan = match plan {
                    KnownOrUnknown::Known(plan) => plan,
                    KnownOrUnknown::Unknown(_) => {
                        // If we get a plan that we don't recognize, fall back to the Free plan.
                        Plan::ZedFree
                    }
                };

                (organization_id, plan)
            })
            .collect();
        self.configuration_by_organization =
            response.configuration_by_organization.into_iter().collect();

        self.edit_prediction_usage = Some(EditPredictionUsage(RequestUsage {
            limit: response.plan.usage.edit_predictions.limit,
            amount: response.plan.usage.edit_predictions.used as i32,
        }));
        self.plan_info = Some(response.plan);
        cx.emit(Event::PrivateUserInfoUpdated);
    }

    fn handle_message_to_client(this: WeakEntity<Self>, message: &MessageToClient, cx: &App) {
        match message {
            MessageToClient::UserUpdated => {}
            MessageToClient::NotificationsUpdated => return,
        }

        cx.spawn(async move |cx| {
            let (cloud_client, system_id) = cx
                .update(|cx| {
                    this.read_with(cx, |this, _cx| {
                        this.client.upgrade().map(|client| {
                            let system_id = client.telemetry().system_id().map(|id| id.to_string());
                            (client.cloud_client(), system_id)
                        })
                    })
                })?
                .ok_or(anyhow::anyhow!("Failed to get Cloud client"))?;

            let response = cloud_client.get_authenticated_user(system_id).await?;
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.update_authenticated_user(response, cx);
                })
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

>>>>>>> ed5cb101cafb7dfe34a7be82b0a32e7dc4cd2982
    pub fn watch_current_user(&self) -> watch::Receiver<Option<Arc<User>>> {
        self.current_user.clone()
    }
}
