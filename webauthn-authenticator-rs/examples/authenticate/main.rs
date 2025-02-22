#[macro_use]
extern crate tracing;

use std::fs::OpenOptions;
use std::io::{stdin, stdout, Write};

use clap::clap_derive::ValueEnum;
use clap::{Args, Parser, Subcommand};
use futures::executor::block_on;
use webauthn_authenticator_rs::ctap2::CtapAuthenticator;
use webauthn_authenticator_rs::prelude::Url;
#[cfg(feature = "cable")]
use webauthn_authenticator_rs::prelude::WebauthnCError;
use webauthn_authenticator_rs::softtoken::{SoftToken, SoftTokenFile};
use webauthn_authenticator_rs::transport::*;
use webauthn_authenticator_rs::types::CableRequestType;
use webauthn_authenticator_rs::ui::{Cli, UiCallback};
use webauthn_authenticator_rs::AuthenticatorBackend;
use webauthn_rs_core::proto::RequestAuthenticationExtensions;
use webauthn_rs_core::WebauthnCore as Webauthn;
use webauthn_rs_proto::{AttestationConveyancePreference, COSEAlgorithm, UserVerificationPolicy};

#[derive(Debug, clap::Parser)]
#[clap(about = "Register and authenticate test")]
pub struct CliParser {
    /// Provider to use.
    #[clap(subcommand)]
    provider: Provider,

    /// User verification policy for the request.
    #[clap(short, long, value_enum, default_value_t)]
    verification_policy: UvPolicy,
}

#[derive(ValueEnum, Clone, Default, Debug)]
pub enum UvPolicy {
    Discouraged,
    #[default]
    Preferred,
    Required,
}

impl From<UvPolicy> for UserVerificationPolicy {
    fn from(value: UvPolicy) -> Self {
        match value {
            UvPolicy::Discouraged => UserVerificationPolicy::Discouraged_DO_NOT_USE,
            UvPolicy::Preferred => UserVerificationPolicy::Preferred,
            UvPolicy::Required => UserVerificationPolicy::Required,
        }
    }
}

fn select_transport<U: UiCallback>(ui: &U) -> impl AuthenticatorBackend + '_ {
    let mut reader = AnyTransport::new().unwrap();
    info!("Using reader: {:?}", reader);

    match reader.tokens() {
        Ok(mut tokens) => {
            while let Some(card) = tokens.pop() {
                let auth = block_on(CtapAuthenticator::new(card, ui));

                if let Some(auth) = auth {
                    return auth;
                }
            }
        }
        Err(e) => panic!("Error: {e:?}"),
    }

    panic!("No tokens available!");
}

#[derive(Debug, Args, Clone)]
pub struct SoftTokenOpt {
    /// Path to serialised key data, created by the softtoken example.
    ///
    /// If not supplied, creates a temporary key in memory.
    #[clap()]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
enum Provider {
    /// Software token provider
    SoftToken(SoftTokenOpt),

    /// CtapAuthenticator using Transport/Token backends (NFC, USB HID)
    ///
    /// Requires administrative permissions on Windows.
    Ctap,

    #[cfg(feature = "cable")]
    /// caBLE/Hybrid authenticator, using a QR code, BTLE and Websockets.
    ///
    /// This requires Bluetooth permission - see the
    /// [webauthn_authenticator_rs::cable] documentation for more information.
    Cable,

    #[cfg(feature = "u2fhid")]
    /// Mozilla webauthn-authenticator-rs provider, supporting USB HID only.
    Mozilla,

    #[cfg(feature = "win10")]
    /// Windows 10 WebAuthn API, supporting BTLE, NFC and USB HID.
    Win10,
}

impl Provider {
    #[allow(unused_variables)]
    async fn connect_provider<'a, U: UiCallback>(
        &self,
        request_type: CableRequestType,
        ui: &'a U,
    ) -> Box<dyn AuthenticatorBackend + 'a> {
        match self {
            Provider::SoftToken(o) => {
                if let Some(path) = &o.path {
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(false)
                        .open(path)
                        .unwrap();
                    Box::new(SoftTokenFile::open(file).unwrap())
                } else {
                    Box::new(SoftToken::new().unwrap().0)
                }
            }
            Provider::Ctap => Box::new(select_transport(ui)),
            #[cfg(feature = "cable")]
            Provider::Cable => Box::new(
                webauthn_authenticator_rs::cable::connect_cable_authenticator(request_type, ui)
                    .await
                    .map_err(|e| {
                        if e == WebauthnCError::PermissionDenied {
                            println!("Permission denied: please grant Bluetooth permissions to your terminal app.");
                            println!("See the webauthn_authenticator_rs::cable module documentation for more info.")
                        }
                        e
                    })
                    .unwrap(),
            ),
            #[cfg(feature = "u2fhid")]
            Provider::Mozilla => Box::<webauthn_authenticator_rs::u2fhid::U2FHid>::default(),
            #[cfg(feature = "win10")]
            Provider::Win10 => Box::<webauthn_authenticator_rs::win10::Win10>::default(),
        }
    }
}

#[tokio::main]
async fn main() {
    let opt = CliParser::parse();

    tracing_subscriber::fmt::init();
    let ui = Cli {};
    let provider = opt.provider;
    let mut u = provider
        .connect_provider(CableRequestType::MakeCredential, &ui)
        .await;

    // WARNING: don't use this as an example of how to use the library!
    let wan = Webauthn::new_unsafe_experts_only(
        "https://localhost:8080/auth",
        "localhost",
        vec![url::Url::parse("https://localhost:8080").unwrap()],
        Some(1),
        None,
        None,
    );

    let unique_id = [
        158, 170, 228, 89, 68, 28, 73, 194, 134, 19, 227, 153, 107, 220, 150, 238,
    ];
    let name = "william";

    let (chal, reg_state) = wan
        .generate_challenge_register_options(
            &unique_id,
            name,
            name,
            AttestationConveyancePreference::None,
            Some(opt.verification_policy.into()),
            None,
            None,
            COSEAlgorithm::secure_algs(),
            false,
            None,
            false,
        )
        .unwrap();

    info!("🍿 challenge -> {:x?}", chal);

    let r = u
        .perform_register(
            Url::parse("https://localhost:8080").unwrap(),
            chal.public_key,
            60_000,
        )
        .unwrap();

    let cred = wan.register_credential(&r, &reg_state, None).unwrap();

    trace!(?cred);
    drop(u);
    let mut buf = String::new();
    println!("WARNING: Some NFC keys need to be power-cycled before you can authenticate.");
    println!("Press ENTER to authenticate, or Ctrl-C to abort");
    stdout().flush().ok();
    stdin().read_line(&mut buf).expect("Cannot read stdin");

    loop {
        u = provider
            .connect_provider(CableRequestType::GetAssertion, &ui)
            .await;
        let (chal, auth_state) = wan
            .generate_challenge_authenticate(
                vec![cred.clone()],
                Some(RequestAuthenticationExtensions {
                    appid: Some("example.app.id".to_string()),
                    uvm: None,
                    hmac_get_secret: None,
                }),
            )
            .unwrap();

        let r = u
            .perform_auth(
                Url::parse("https://localhost:8080").unwrap(),
                chal.public_key,
                60_000,
            )
            .map_err(|e| {
                error!("Error -> {:x?}", e);
                e
            });
        trace!(?r);

        if let Ok(r) = r {
            let auth_res = wan
                .authenticate_credential(&r, &auth_state)
                .expect("webauth authentication denied");

            info!("auth_res -> {:x?}", auth_res);
        }

        drop(u);
        let mut buf = String::new();
        println!("Press ENTER to try again, or Ctrl-C to abort");
        stdout().flush().ok();
        stdin().read_line(&mut buf).expect("Cannot read stdin");
    }
}
