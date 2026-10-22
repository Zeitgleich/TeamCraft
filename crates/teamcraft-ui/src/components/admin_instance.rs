use leptos::prelude::*;
use teamcraft_proto::{AdminInstanceResponse, PushProviderState};

use crate::admin::format_bytes;
use crate::clock::{format_relative, now_ms};
use crate::components::admin_console::SectionHeader;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// An SFU that has not been in touch for this long counts as silent.
const SFU_SILENT_AFTER_MS: i64 = 120_000;

/// What the instance runs on: media server, push providers, storage and telephony (admin.md §3.4).
#[component]
pub fn InstanceSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let status = Memo::new(move |_| state.read(|backend| backend.instance_status()));

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-instance-title"))
      subtitle=Signal::derive(move || i18n.t("admin-instance-subtitle"))
    />
    {move || match status.get() {
      None => view! { <p class="empty-state">{move || i18n.t("instance-loading")}</p> }.into_any(),
      Some(instance) => view! { <InstanceCards instance=instance /> }.into_any(),
    }}
  }
}

#[component]
fn InstanceCards(instance: AdminInstanceResponse) -> impl IntoView {
  let i18n = use_i18n();
  let now = now_ms();
  let sfu = instance.sfu.clone();
  let sfu_health = sfu_health(&sfu, now);

  view! {
    <div class="instance-grid">
      <section class="instance-card" aria-labelledby="instance-sfu">
        <h3 id="instance-sfu"><Icon name=IconName::Video /> {move || i18n.t("instance-sfu")}</h3>
        <p>
          <span class=format!("health-dot health-{}", sfu_health.slug()) aria-hidden="true"></span>
          <strong>{move || i18n.t(&format!("instance-sfu-{}", sfu_health.slug()))}</strong>
        </p>
        {sfu.maybe_url.clone().map(|url| view! { <p class="mono small" translate="no">{url}</p> })}
        <dl class="meta-list">
          <dt>{move || i18n.t("instance-sfu-registration")}</dt>
          <dd>{move || i18n.t(if sfu.is_reporting_enabled { "instance-yes" } else { "instance-no" })}</dd>
          <dt>{move || i18n.t("instance-sfu-controls")}</dt>
          <dd>{move || i18n.t(if sfu.has_host_controls { "instance-yes" } else { "instance-no" })}</dd>
          <dt>{move || i18n.t("instance-sfu-contact")}</dt>
          <dd>
            {move || match sfu.maybe_last_contact_at {
              Some(at) => format_relative(at.0, now, i18n.locale().tag()),
              None => i18n.t("instance-never"),
            }}
          </dd>
          <dt>{move || i18n.t("instance-sfu-calls")}</dt>
          <dd>{sfu.active_calls}</dd>
        </dl>
        <p class="muted-text small">{move || i18n.t("instance-sfu-hint")}</p>
      </section>

      <section class="instance-card" aria-labelledby="instance-push">
        <h3 id="instance-push"><Icon name=IconName::Bell /> {move || i18n.t("instance-push")}</h3>
        <table class="data-table">
          <caption class="visually-hidden">{move || i18n.t("instance-push")}</caption>
          <thead>
            <tr>
              <th scope="col">{move || i18n.t("instance-push-provider")}</th>
              <th scope="col">{move || i18n.t("instance-push-state")}</th>
              <th scope="col" class="num">{move || i18n.t("instance-push-devices")}</th>
            </tr>
          </thead>
          <tbody>
            {instance
              .push
              .iter()
              .map(|entry| {
                let provider_key = format!("instance-provider-{}", entry.provider.as_str());
                let state_slug = push_state_slug(&entry.state);
                view! {
                  <tr>
                    <th scope="row">{move || i18n.t(&provider_key)}</th>
                    <td><span class=format!("badge badge-{state_slug}")>{move || i18n.t(&format!("instance-push-{state_slug}"))}</span></td>
                    <td class="num">{entry.device_count}</td>
                  </tr>
                }
              })
              .collect_view()}
          </tbody>
        </table>
        <p class="muted-text small">{move || i18n.t("instance-push-hint")}</p>
      </section>

      <section class="instance-card" aria-labelledby="instance-storage">
        <h3 id="instance-storage"><Icon name=IconName::Layers /> {move || i18n.t("instance-storage")}</h3>
        {if instance.storage.is_configured {
          view! {
            <dl class="meta-list">
              <dt>{move || i18n.t("instance-storage-attachments")}</dt>
              <dd>{format!("{} ({})", format_bytes(instance.storage.attachment_bytes), instance.storage.attachment_count)}</dd>
              <dt>{move || i18n.t("instance-storage-index")}</dt>
              <dd>{format_bytes(instance.storage.index_segment_bytes)}</dd>
            </dl>
            <p class="muted-text small">{move || i18n.t("instance-storage-hint")}</p>
          }
            .into_any()
        } else {
          view! { <p class="muted-text">{move || i18n.t("instance-storage-off")}</p> }.into_any()
        }}
      </section>

      <section class="instance-card" aria-labelledby="instance-telephony">
        <h3 id="instance-telephony"><Icon name=IconName::Phone /> {move || i18n.t("instance-telephony")}</h3>
        <p class="muted-text">{move || i18n.t("instance-telephony-soon")}</p>
        <span class="badge">{move || i18n.t("instance-coming-soon")}</span>
      </section>
    </div>
  }
}

/// How the media server looks from the server's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SfuHealth {
  NotConfigured,
  /// Configured, but it has not reported yet or reporting is off.
  Unknown,
  Healthy,
  Silent,
}

impl SfuHealth {
  fn slug(self) -> &'static str {
    match self {
      Self::NotConfigured => "off",
      Self::Unknown => "unknown",
      Self::Healthy => "ok",
      Self::Silent => "silent",
    }
  }
}

pub fn sfu_health(sfu: &teamcraft_proto::SfuStatus, now: i64) -> SfuHealth {
  if !sfu.is_configured {
    return SfuHealth::NotConfigured;
  }
  match sfu.maybe_last_contact_at {
    None => SfuHealth::Unknown,
    Some(at) if now - at.0 <= SFU_SILENT_AFTER_MS => SfuHealth::Healthy,
    Some(_) => SfuHealth::Silent,
  }
}

fn push_state_slug(state: &PushProviderState) -> &'static str {
  match state {
    PushProviderState::Active => "active",
    PushProviderState::NotConfigured => "not-configured",
    PushProviderState::NotImplemented => "not-implemented",
    PushProviderState::Unknown(_) => "unknown",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::{SfuStatus, UnixMillis};

  fn sfu(is_configured: bool, contact: Option<i64>) -> SfuStatus {
    SfuStatus {
      is_configured,
      maybe_url: None,
      is_reporting_enabled: true,
      has_host_controls: false,
      maybe_last_contact_at: contact.map(UnixMillis),
      active_calls: 0,
    }
  }

  #[test]
  fn the_sfu_is_judged_by_its_last_contact() {
    assert_eq!(sfu_health(&sfu(false, None), 1_000_000), SfuHealth::NotConfigured);
    assert_eq!(sfu_health(&sfu(true, None), 1_000_000), SfuHealth::Unknown);
    assert_eq!(
      sfu_health(&sfu(true, Some(990_000)), 1_000_000),
      SfuHealth::Healthy
    );
    assert_eq!(
      sfu_health(&sfu(true, Some(100_000)), 1_000_000),
      SfuHealth::Silent
    );
  }

  #[test]
  fn every_provider_has_a_wire_name() {
    use teamcraft_proto::PushProvider;
    for provider in [
      PushProvider::WebPush,
      PushProvider::Fcm,
      PushProvider::UnifiedPush,
      PushProvider::Apns,
    ] {
      assert!(!provider.as_str().is_empty());
    }
  }
}
