/*!
Rust X.509 certificate generation utility

This crate provides a way to generate self signed X.509 certificates.

The most simple way of using this crate is by calling the
`generate_simple_self_signed` function.
For more customization abilities, we provide the lower level
`Certificate::from_params` function.

## Example

```
extern crate rcgen;
use rcgen::generate_simple_self_signed;
# fn main () {
// Generate a certificate that's valid for "localhost" and "hello.world.example"
let subject_alt_names = vec!["hello.world.example".to_string(),
	"localhost".to_string()];

let cert = generate_simple_self_signed(subject_alt_names);
println!("{}", cert.serialize_pem());
println!("{}", cert.serialize_private_key_pem());
# }
```
*/

#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate yasna;
extern crate ring;
#[cfg(feature = "pem")]
extern crate pem;
extern crate untrusted;
extern crate chrono;
extern crate bit_vec;

use yasna::Tag;
use yasna::models::ObjectIdentifier;
#[cfg(feature = "pem")]
use pem::Pem;
use ring::digest;
use ring::signature::{EcdsaKeyPair, Ed25519KeyPair, RsaKeyPair};
use ring::rand::SystemRandom;
use ring::signature::KeyPair as RingKeyPair;
use untrusted::Input;
use ring::signature::{self, EcdsaSigningAlgorithm, EdDSAParameters};
use yasna::DERWriter;
use yasna::models::GeneralizedTime;
use chrono::{DateTime, Timelike};
use chrono::{NaiveDate, Utc};
use std::collections::HashMap;
use bit_vec::BitVec;

/// A self signed certificate together with signing keys
pub struct Certificate {
	params :CertificateParams,
	key_pair :KeyPair,
}


/**
KISS function to generate a self signed certificate

Given a set of domain names you want your certificate to be valid for,
this function fills in the other generation parameters with
reasonable defaults and generates a self signed certificate
as output.

## Example

```
extern crate rcgen;
use rcgen::generate_simple_self_signed;
# fn main () {
let subject_alt_names :&[_] = &["hello.world.example".to_string(),
	"localhost".to_string()];

let cert = generate_simple_self_signed(subject_alt_names);
// The certificate is now valid for localhost and the domain "hello.world.example"
println!("{}", cert.serialize_pem());
println!("{}", cert.serialize_private_key_pem());
# }
```
*/
pub fn generate_simple_self_signed(subject_alt_names :impl Into<Vec<String>>) -> Certificate {
	Certificate::from_params(CertificateParams::new(subject_alt_names))
}

// https://tools.ietf.org/html/rfc5280#section-4.1.1

// Example certs usable as reference:
// Uses ECDSA: https://crt.sh/?asn1=607203242

/// pkcs-9-at-extensionRequest in RFC 2985
const OID_PKCS_9_AT_EXTENSION_REQUEST :&[u64] = &[1, 2, 840, 113549, 1, 9, 14];

/// id-at-countryName in RFC 5820
const OID_COUNTRY_NAME :&[u64] = &[2, 5, 4, 6];
/// id-at-organizationName in RFC 5820
const OID_ORG_NAME :&[u64] = &[2, 5, 4, 10];
/// id-at-commonName in RFC 5820
const OID_COMMON_NAME :&[u64] = &[2, 5, 4, 3];

// https://tools.ietf.org/html/rfc5480#section-2.1.1
const OID_EC_PUBLIC_KEY :&[u64] = &[1, 2, 840, 10045, 2, 1];
const OID_EC_SECP_256_R1 :&[u64] = &[1, 2, 840, 10045, 3, 1, 7];
const OID_EC_SECP_384_R1 :&[u64] = &[1, 3, 132, 0, 34];

// rsaEncryption in RFC 4055
const OID_RSA_ENCRYPTION :&[u64] = &[1, 2, 840, 113549, 1, 1, 1];

// https://tools.ietf.org/html/rfc5280#appendix-A.2
// https://tools.ietf.org/html/rfc5280#section-4.2.1.6
const OID_SUBJECT_ALT_NAME :&[u64] = &[2, 5, 29, 17];

// https://tools.ietf.org/html/rfc5280#section-4.2.1.9
const OID_BASIC_CONSTRAINTS :&[u64] = &[2, 5, 29, 19];

// https://tools.ietf.org/html/rfc5280#section-4.2.1.2
const OID_SUBJECT_KEY_IDENTIFIER :&[u64] = &[2, 5, 29, 14];

// id-pe-acmeIdentifier in
// https://www.iana.org/assignments/smi-numbers/smi-numbers.xhtml#smi-numbers-1.3.6.1.5.5.7.1
const OID_PE_ACME :&[u64] = &[1, 3, 6, 1, 5, 5, 7, 1, 31];

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
#[allow(missing_docs)]
/// The attribute type of a distinguished name entry
pub enum DnType {
	CountryName,
	OrganizationName,
	CommonName,
	#[doc(hidden)]
	_Nonexhaustive,
}

impl DnType {
	fn to_oid(&self) -> ObjectIdentifier {
		let sl = match self {
			DnType::CountryName => OID_COUNTRY_NAME,
			DnType::OrganizationName => OID_ORG_NAME,
			DnType::CommonName => OID_COMMON_NAME,
			DnType::_Nonexhaustive => unimplemented!(),
		};
		ObjectIdentifier::from_slice(sl)
	}
}

#[derive(Debug, PartialEq, Eq, Clone)]
/**
Distinguished name used e.g. for the issuer and subject fields of a certificate

A distinguished name is a set of (attribute type, attribute value) tuples.

See also the RFC 5280 sections on the [issuer](https://tools.ietf.org/html/rfc5280#section-4.1.2.4)
and [subject](https://tools.ietf.org/html/rfc5280#section-4.1.2.6) fields.
*/
pub struct DistinguishedName {
	entries :HashMap<DnType, String>,
}

impl DistinguishedName {
	/// Creates a new, empty distinguished name
	pub fn new() -> Self {
		Self {
			entries : HashMap::new(),
		}
	}
	/// Inserts a new attribute that consists of type and name
	pub fn push(&mut self, ty :DnType, s :impl Into<String>) {
		self.entries.insert(ty, s.into());
	}
}

/// Parameters used for certificate generation
#[allow(missing_docs)]
pub struct CertificateParams {
	pub alg :&'static SignatureAlgorithm,
	pub not_before :DateTime<Utc>,
	pub not_after :DateTime<Utc>,
	pub serial_number :Option<u64>,
	pub subject_alt_names :Vec<String>,
	pub distinguished_name :DistinguishedName,
	pub is_ca :IsCa,
	pub custom_extensions :Vec<CustomExtension>,
	/// The certificate's key pair, a new random key pair will be generated if this is `None`
	pub key_pair :Option<KeyPair>,
	// To make the struct non-exhaustive
	_hidden :(),
}

impl Default for CertificateParams {
	fn default() -> Self {
		// not_before and not_after set to reasonably long dates
		let not_before = date_time_ymd(1975, 01, 01);
		let not_after = date_time_ymd(4096, 01, 01);
		let mut distinguished_name = DistinguishedName::new();
		distinguished_name.push(DnType::CommonName, "rcgen self signed cert");
		CertificateParams {
			alg : &PKCS_ECDSA_P256_SHA256,
			not_before,
			not_after,
			serial_number : None,
			subject_alt_names : Vec::new(),
			distinguished_name,
			is_ca : IsCa::SelfSignedOnly,
			custom_extensions : Vec::new(),
			key_pair : None,
			_hidden :(),
		}
	}
}

/// Whether the certificate is allowed to sign other certificates
pub enum IsCa {
	/// The certificate can only sign itself
	SelfSignedOnly,
	/// The certificate may be used to sign other certificates
	Ca(BasicConstraints),
}

/// The path length constraint (only relevant for CA certificates)
///
/// Sets an optional upper limit on the length of the intermediate certificate chain
/// length allowed for this CA certificate (not including the end entity certificate).
pub enum BasicConstraints {
	/// No constraint
	Unconstrained,
	/// Constrain to the contained number of intermediate certificates
	Constrained(u8),
}

impl CertificateParams {
	/// Generate certificate parameters with reasonable defaults
	pub fn new(subject_alt_names :impl Into<Vec<String>>) -> Self {
		CertificateParams {
			subject_alt_names : subject_alt_names.into(),
			.. Default::default()
		}
	}
}

/// A custom extension of a certificate, as specified in
/// [RFC 5280](https://tools.ietf.org/html/rfc5280#section-4.2)
pub struct CustomExtension {
	oid :Vec<u64>,
	critical :bool,
	content :Vec<u8>,
}

impl CustomExtension {
	/// Creates a new acmeIdentifier extension for ACME TLS-ALPN-01
	/// as specified in [draft-ietf-acme-tls-alpn-05](https://tools.ietf.org/html/draft-ietf-acme-tls-alpn-05#section-3)
	///
	/// Panics if the passed `sha_digest` parameter doesn't hold 32 bytes (256 bits).
	pub fn new_acme_identifier(sha_digest :&[u8]) -> Self {
		assert_eq!(sha_digest.len(), 32, "wrong size of sha_digest");
		let content = yasna::construct_der(|writer| {
			writer.write_bytes(sha_digest);
		});
		Self {
			oid : OID_PE_ACME.to_owned(),
			critical : true,
			content,
		}
	}
	/// Create a new custom extension
	pub fn from_oid_content(oid :&[u64], content :Vec<u8>) -> Self {
		Self {
			oid : oid.to_owned(),
			critical : false,
			content,
		}
	}
	/// Sets the criticality flag of the extension.
	pub fn set_criticality(&mut self, criticality :bool) {
		self.critical = criticality;
	}
}

/// Helper to obtain a DateTime from year, month, day values
///
/// The year, month, day values are assumed to be in UTC.
///
/// This helper function serves two purposes: first, so that you don't
/// have to import the chrono crate yourself in order to specify date
/// information, second so that users don't have to type unproportionately
/// long code just to generate an instance of `DateTime<Utc>`.
pub fn date_time_ymd(year :i32, month :u32, day :u32) -> DateTime<Utc> {
	let naive_dt = NaiveDate::from_ymd(year, month, day).and_hms_milli(0, 0, 0, 0);
	DateTime::<Utc>::from_utc(naive_dt, Utc)
}

fn dt_to_generalized(dt :&DateTime<Utc>) -> GeneralizedTime {
	let mut date_time = *dt;
	// Set nanoseconds to zero (or to one leap second if there is a leap second)
	// This is needed because the GeneralizedTime serializer would otherwise
	// output fractional values which RFC 5820 explicitly forbode [1].
	// [1]: https://tools.ietf.org/html/rfc5280#section-4.1.2.5.2
	let nanos = if date_time.nanosecond() >= 1_000_000 {
		1_000_000
	} else {
		0
	};
	date_time = date_time.with_nanosecond(nanos).unwrap();
	GeneralizedTime::from_datetime::<Utc>(&date_time)
}

impl Certificate {
	/// Generates a new certificate from the given parameters
	pub fn from_params(mut params :CertificateParams) -> Self {
		let key_pair = if let Some(key_pair) = params.key_pair.take() {
			key_pair
		} else {
			KeyPair::generate(&params.alg)
		};

		Certificate {
			params,
			key_pair,
		}
	}
	fn write_name(&self, writer :DERWriter, ca :&Certificate) {
		writer.write_sequence(|writer| {
			writer.next().write_set(|writer| {
				for (ty, content) in ca.params.distinguished_name.entries.iter() {
					writer.next().write_sequence(|writer| {
						writer.next().write_oid(&ty.to_oid());
						writer.next().write_utf8_string(content);
					});
				}
			});
		});
	}
    fn write_request(&self, writer :DERWriter) {
		writer.write_sequence(|writer| {
			// Write version
			writer.next().write_u8(0);
			// Write issuer
			writer.next().write_sequence(|writer| {
				for (ty, content) in self.params.distinguished_name.entries.iter() {
					writer.next().write_set(|writer| {
						writer.next().write_sequence(|writer| {
							writer.next().write_oid(&ty.to_oid());
							writer.next().write_utf8_string(content);
						});
					});
				}
			});
			// Write subjectPublicKeyInfo
			writer.next().write_sequence(|writer| {
				self.params.alg.write_oids_sign_alg(writer.next());
				let public_key = self.key_pair.public_key();
				let pkbs = BitVec::from_bytes(&public_key);
				writer.next().write_bitvec(&pkbs);
			});
			// Write extensions
			writer.next().write_tagged(Tag::context(0), |writer| {
				writer.write_sequence(|writer| {
					let oid = ObjectIdentifier::from_slice(OID_PKCS_9_AT_EXTENSION_REQUEST);
					writer.next().write_oid(&oid);
					writer.next().write_set(|writer| {
						writer.next().write_sequence(|writer| {
							// Write subject_alt_names
							writer.next().write_sequence(|writer| {
								let oid = ObjectIdentifier::from_slice(OID_SUBJECT_ALT_NAME);
								writer.next().write_oid(&oid);
								let bytes = yasna::construct_der(|writer| {
									writer.write_sequence(|writer| {
										for san in self.params.subject_alt_names.iter() {
											// All subject alt names are dNSName.
											const TAG_DNS_NAME :u64 = 2;
											writer.next().write_tagged_implicit(Tag::context(TAG_DNS_NAME), |writer| {
												writer.write_utf8_string(san);
											});
										}
									});
								});
								writer.next().write_bytes(&bytes);
							});
						});
					});
				});
			});
		});
	}
	fn write_cert(&self, writer :DERWriter, ca :&Certificate) {
		writer.write_sequence(|writer| {
			// Write version
			writer.next().write_tagged(Tag::context(0), |writer| {
				writer.write_u8(2);
			});
			// Write serialNumber
			let serial = self.params.serial_number.unwrap_or(42);
			writer.next().write_u64(serial);
			// Write signature
			self.params.alg.write_alg_ident(writer.next());
			// Write issuer
			self.write_name(writer.next(), ca);
			// Write validity
			writer.next().write_sequence(|writer| {
				// Not before
				let nb_gt = dt_to_generalized(&self.params.not_before);
				writer.next().write_generalized_time(&nb_gt);
				// Not after
				let na_gt = dt_to_generalized(&self.params.not_after);
				writer.next().write_generalized_time(&na_gt);
			});
			// Write subject
			self.write_name(writer.next(), self);
			// Write subjectPublicKeyInfo
			writer.next().write_sequence(|writer| {
				self.params.alg.write_oids_sign_alg(writer.next());
				let public_key = self.key_pair.public_key();
				let pkbs = BitVec::from_bytes(&public_key);
				writer.next().write_bitvec(&pkbs);
			});
			// write extensions
			writer.next().write_tagged(Tag::context(3), |writer| {
				writer.write_sequence(|writer| {
					// Write subject_alt_names
					writer.next().write_sequence(|writer| {
						let oid = ObjectIdentifier::from_slice(OID_SUBJECT_ALT_NAME);
						writer.next().write_oid(&oid);
						let bytes = yasna::construct_der(|writer| {
							writer.write_sequence(|writer|{
								for san in self.params.subject_alt_names.iter() {
									// All subject alt names are dNSName.
									const TAG_DNS_NAME :u64 = 2;
									writer.next().write_tagged_implicit(Tag::context(TAG_DNS_NAME), |writer| {
										writer.write_utf8_string(san);
									});
								}
							});
						});
						writer.next().write_bytes(&bytes);
					});
					if let IsCa::Ca(ref constraint) = self.params.is_ca {
						// Write subject_key_identifier
						writer.next().write_sequence(|writer| {
							let oid = ObjectIdentifier::from_slice(OID_SUBJECT_KEY_IDENTIFIER);
							writer.next().write_oid(&oid);
							let digest = digest::digest(&self.params.alg.digest_alg, self.key_pair.public_key().as_ref());
							writer.next().write_bytes(&digest.as_ref());
						});
						// Write basic_constraints
						writer.next().write_sequence(|writer| {
							let oid = ObjectIdentifier::from_slice(OID_BASIC_CONSTRAINTS);
							writer.next().write_oid(&oid);
							let bytes = yasna::construct_der(|writer| {
								writer.write_sequence(|writer| {
									writer.next().write_bool(true); // cA flag
									if let BasicConstraints::Constrained(path_len_constraint) = constraint {
										writer.next().write_u8(*path_len_constraint);
									}
								});
							});
							writer.next().write_bytes(&bytes);
						});
					}
					// Write the custom extensions
					for ext in &self.params.custom_extensions {
						writer.next().write_sequence(|writer| {
							let oid = ObjectIdentifier::from_slice(&ext.oid);
							writer.next().write_oid(&oid);
							// If the extension is critical, we should signal this.
							// It's false by default so we don't need to write anything
							// if the extension is not critical.
							if ext.critical {
								writer.next().write_bool(true);
							}
							writer.next().write_bytes(&ext.content);
						});
					}
				});
			});
		})
	}
	/// Serializes the certificate to the binary DER format
	pub fn serialize_der(&self) -> Vec<u8> {
		self.serialize_der_with_signer(&self)
	}
	/// Serializes the certificate, signed with another certificate's key, in binary DER format
	pub fn serialize_der_with_signer(&self, ca :&Certificate) -> Vec<u8> {
		yasna::construct_der(|writer| {
			writer.write_sequence(|writer| {

				let tbs_cert_list_serialized = yasna::construct_der(|writer| {
					self.write_cert(writer, ca);
				});
				// Write tbsCertList
				writer.next().write_der(&tbs_cert_list_serialized);

				// Write signatureAlgorithm
				self.params.alg.write_alg_ident(writer.next());

				// Write signature
				ca.key_pair.sign(&tbs_cert_list_serialized, writer.next());
			})
		})
	}
    /// Serializes a certificate signing request in binary DER format
    pub fn serialize_request_der(&self) -> Vec<u8> {
		yasna::construct_der(|writer| {
			writer.write_sequence(|writer| {
				let cert_data = yasna::construct_der(|writer| {
					self.write_request(writer);
				});
				writer.next().write_der(&cert_data);

				// Write signatureAlgorithm
				self.params.alg.write_alg_ident(writer.next());

				// Write signature
				self.key_pair.sign(&cert_data, writer.next());
			});
		})
	}
	/// Serializes the certificate to the ASCII PEM format
	#[cfg(feature = "pem")]
	pub fn serialize_pem(&self) -> String {
		let p = Pem {
			tag : "CERTIFICATE".to_string(),
			contents : self.serialize_der(),
		};
		pem::encode(&p)
	}
	/// Serializes the certificate, signed with another certificate's key, to the ASCII PEM format
	#[cfg(feature = "pem")]
	pub fn serialize_pem_with_signer(&self, ca :&Certificate) -> String {
		let p = Pem {
			tag : "CERTIFICATE".to_string(),
			contents : self.serialize_der_with_signer(ca),
		};
		pem::encode(&p)
	}
	/// Serializes the certificate signing request to the ASCII PEM format
	#[cfg(feature = "pem")]
	pub fn serialize_request_pem(&self) -> String {
		let p = Pem {
			tag : "CERTIFICATE REQUEST".to_string(),
			contents : self.serialize_request_der(),
		};
		pem::encode(&p)
	}
	/// Serializes the private key in PKCS#8 format
	pub fn serialize_private_key_der(&self) -> Vec<u8> {
		self.key_pair.serialize_der()
	}
	/// Serializes the private key in PEM format
	#[cfg(feature = "pem")]
	pub fn serialize_private_key_pem(&self) -> String {
		self.key_pair.serialize_pem()
	}
}

enum SignAlgo {
	EcDsa(&'static EcdsaSigningAlgorithm),
	EdDsa(&'static EdDSAParameters),
	Rsa(),
}

/// A key pair used to sign certificates and CSRs
pub enum KeyPair {
	/// A Ecdsa key pair
	EcKp(EcdsaKeyPair, Vec<u8>),
	/// A Ed25519 key pair
	EdKp(Ed25519KeyPair, Vec<u8>),
	/// A RSA key pair
	Rsa(RsaKeyPair, Vec<u8>),
}

impl KeyPair {
	/// Parses the key pair from the ASCII PEM format
	#[cfg(feature = "pem")]
	pub fn from_pem(pem_str :&str) -> Self {
		let private_key = pem::parse(pem_str).unwrap();
		let private_key_der :&[_] = &private_key.contents;
		private_key_der.into()
	}
}

impl From<&[u8]> for KeyPair {
	fn from(pkcs8 :&[u8]) -> KeyPair {
		let input = Input::from(pkcs8);
		let pkcs8_vec = std::iter::FromIterator::from_iter(pkcs8.iter().cloned());

		if let Ok(edkp) = Ed25519KeyPair::from_pkcs8_maybe_unchecked(input) {
			KeyPair::EdKp(edkp, pkcs8_vec)
		} else if let Ok(eckp) = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_ASN1_SIGNING, input) {
			KeyPair::EcKp(eckp, pkcs8_vec)
		} else if let Ok(eckp) = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P384_SHA384_ASN1_SIGNING, input) {
			KeyPair::EcKp(eckp, pkcs8_vec)
		} else if let Ok(rsakp) = RsaKeyPair::from_pkcs8(input) {
			KeyPair::Rsa(rsakp, pkcs8_vec)
		} else {
			panic!("Could not parse key pair {:?}", RsaKeyPair::from_pkcs8(input));
		}
	}
}

impl KeyPair {
	/// Generate a new random key pair for the specified signature algorithm
	pub fn generate(alg :&SignatureAlgorithm) -> Self {
		let system_random = SystemRandom::new();
		match alg.sign_alg {
			SignAlgo::EcDsa(sign_alg) => {
				let key_pair_doc = EcdsaKeyPair::generate_pkcs8(sign_alg, &system_random).unwrap();
				let key_pair_serialized = key_pair_doc.as_ref().to_vec();

				let key_pair = EcdsaKeyPair::from_pkcs8(&sign_alg, Input::from(&&key_pair_doc.as_ref())).unwrap();
				KeyPair::EcKp(key_pair, key_pair_serialized)
			},
			SignAlgo::EdDsa(_sign_alg) => {
				let key_pair_doc = Ed25519KeyPair::generate_pkcs8(&system_random).unwrap();
				let key_pair_serialized = key_pair_doc.as_ref().to_vec();

				let key_pair = Ed25519KeyPair::from_pkcs8(Input::from(&&key_pair_doc.as_ref())).unwrap();
				KeyPair::EdKp(key_pair, key_pair_serialized)
			},
			// Ring doesn't have RSA key generation yet:
			// https://github.com/briansmith/ring/issues/219
			// https://github.com/briansmith/ring/pull/733
			SignAlgo::Rsa() => panic!("Key generation for RSA not available."),
		}
	}
	fn public_key(&self) -> &[u8] {
		match self {
			KeyPair::EcKp(kp, _) => kp.public_key().as_ref(),
			KeyPair::EdKp(kp, _) => kp.public_key().as_ref(),
			KeyPair::Rsa(kp, _) => kp.public_key().as_ref(),
		}
	}
	fn sign(&self, msg :&[u8], writer :DERWriter) {
		match self {
			KeyPair::EcKp(kp, _) => {
				let msg_input = Input::from(&msg);
				let system_random = SystemRandom::new();
				let signature = kp.sign(&system_random, msg_input).unwrap();
				let sig = BitVec::from_bytes(&signature.as_ref());
				writer.write_bitvec(&sig);
			},
			KeyPair::EdKp(kp, _) => {
				let signature = kp.sign(msg);
				let sig = BitVec::from_bytes(&signature.as_ref());
				writer.write_bitvec(&sig);
			},
			KeyPair::Rsa(kp, _) => {
				let system_random = SystemRandom::new();
				let mut signature = vec![0; kp.public_modulus_len()];
				kp.sign(&signature::RSA_PKCS1_SHA256, &system_random,
					msg, &mut signature).unwrap();
				let sig = BitVec::from_bytes(&signature.as_ref());
				writer.write_bitvec(&sig);
			},
		}
	}
	/// Serializes the private key in PKCS#8 format
	pub fn serialize_der(&self) -> Vec<u8> {
		let serialized_key = match self {
			KeyPair::EcKp(_, ref serialized_key) => serialized_key,
			KeyPair::EdKp(_, ref serialized_key) => serialized_key,
			KeyPair::Rsa(_, ref serialized_key) => serialized_key,
		};
		serialized_key.clone()
	}
	/// Serializes the private key in PEM format
	#[cfg(feature = "pem")]
	pub fn serialize_pem(&self) -> String {
		let p = Pem {
			tag : "PRIVATE KEY".to_string(),
			contents : self.serialize_der(),
		};
		pem::encode(&p)
	}
}

/// Signature algorithm type
pub struct SignatureAlgorithm {
	oids_sign_alg :&'static [&'static [u64]],
	sign_alg :SignAlgo,
	digest_alg :&'static ring::digest::Algorithm,
	oid_components :&'static [u64],
	write_null_params :bool,
}


/// RSA signing with PKCS#1 1.5 padding and SHA-256 hashing as per [RFC 4055](https://tools.ietf.org/html/rfc4055)
pub static PKCS_RSA_SHA256 :SignatureAlgorithm = SignatureAlgorithm {
	oids_sign_alg :&[&OID_RSA_ENCRYPTION],
	sign_alg :SignAlgo::Rsa(),
	digest_alg :&digest::SHA256,
	// sha256WithRSAEncryption in RFC 4055
	oid_components : &[1, 2, 840, 113549, 1, 1, 11],
	write_null_params : true,
};


/// ECDSA signing using the P-256 curves and SHA-256 hashing as per [RFC 5758](https://tools.ietf.org/html/rfc5758#section-3.2)
pub static PKCS_ECDSA_P256_SHA256 :SignatureAlgorithm = SignatureAlgorithm {
	oids_sign_alg :&[&OID_EC_PUBLIC_KEY, &OID_EC_SECP_256_R1],
	sign_alg :SignAlgo::EcDsa(&signature::ECDSA_P256_SHA256_ASN1_SIGNING),
	digest_alg :&digest::SHA256,
	/// ecdsa-with-SHA256 in RFC 5758
	oid_components : &[1, 2, 840, 10045, 4, 3, 2],
	write_null_params : false,
};

/// ECDSA signing using the P-384 curves and SHA-384 hashing as per [RFC 5758](https://tools.ietf.org/html/rfc5758#section-3.2)
pub static PKCS_ECDSA_P384_SHA384 :SignatureAlgorithm = SignatureAlgorithm {
	oids_sign_alg :&[&OID_EC_PUBLIC_KEY, &OID_EC_SECP_384_R1],
	sign_alg :SignAlgo::EcDsa(&signature::ECDSA_P384_SHA384_ASN1_SIGNING),
	digest_alg :&digest::SHA384,
	/// ecdsa-with-SHA384 in RFC 5758
	oid_components : &[1, 2, 840, 10045, 4, 3, 3],
	write_null_params : false,
};

// TODO PKCS_ECDSA_P521_SHA512 https://github.com/briansmith/ring/issues/824

/// ED25519 curve signing as per [RFC 8410](https://tools.ietf.org/html/rfc8410)
pub static PKCS_ED25519 :SignatureAlgorithm = SignatureAlgorithm {
	/// id-Ed25519 in RFC 8410
	oids_sign_alg :&[&[1, 3, 101, 112]],
	sign_alg :SignAlgo::EdDsa(&signature::ED25519),
	digest_alg :&digest::SHA512,
	/// id-Ed25519 in RFC 8410
	oid_components : &[1, 3, 101, 112],
	write_null_params : false,
};

// Signature algorithm IDs as per https://tools.ietf.org/html/rfc4055
impl SignatureAlgorithm {
	fn alg_ident_oid(&self) -> ObjectIdentifier {
		ObjectIdentifier::from_slice(self.oid_components)
	}
	fn write_alg_ident(&self, writer :DERWriter) {
		writer.write_sequence(|writer| {
			writer.next().write_oid(&self.alg_ident_oid());
			if self.write_null_params {
				writer.next().write_null();
			}
		});
	}
	fn write_oids_sign_alg(&self, writer :DERWriter) {
		writer.write_sequence(|writer| {
			for oid in self.oids_sign_alg {
				let oid = ObjectIdentifier::from_slice(oid);
				writer.next().write_oid(&oid);
			}
			if self.write_null_params {
				writer.next().write_null();
			}
		});
	}
}
