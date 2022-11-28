use std::{io::{BufReader, self, BufRead, BufWriter, Write}, fs};
use ethabi::ParamType;
use hex::ToHex;
use serde::Serialize;

use tinytemplate::{TinyTemplate, format_unescaped};
use clap::Parser;
use convert_case::{Case, Casing};
use itertools::Itertools;
use sha3::{Digest, Keccak256};

#[derive(Parser, Debug)]
struct Args {
    /// Input filename or stdin if empty
    #[arg(long, short)]
    input: Option<String>,

    /// Output filename or stdout if empty
    #[arg(long, short)]
    output: Option<String>,

    /// Ink module name to generate
    #[arg(long, short)]
    module_name: String,

    /// EVM ID to use in module
    #[arg(long, short, default_value = "0x0F")]
    evm_id: String,
}

static MODULE_TEMPLATE: &'static str = r#"
//! This file was autogenerated by Sumi
#![cfg_attr(not(feature = "std"), no_std)]

use ink_lang as ink;
pub use self::{name}::\{
    {name | capitalize},
    {name | capitalize}Ref,
};

/// EVM ID from runtime
const EVM_ID: u8 = {evm_id};

/// The EVM ERC20 delegation contract.
#[ink::contract(env = xvm_environment::XvmDefaultEnvironment)]
mod {name} \{
{{ for function in functions }}
    // Selector for `{function.selector}`
    const {function.name | upper_snake}_SELECTOR: [u8; 4] = hex!["{function.selector_hash}"];
{{ endfor }}

    use ethabi::\{
        ethereum_types::\{
            H160,
            U256,
        },
        Token,
    };
    use hex_literal::hex;
    use ink_prelude::vec::Vec;

    #[ink(storage)]
    pub struct {name | capitalize} \{
        evm_address: H160,
    }

    impl {name | capitalize} \{
        /// Create new abstraction from given contract address.
        #[ink(constructor)]
        pub fn new(evm_address: H160) -> Self \{
            Self \{ evm_address }
        }

{{ for function in functions }}
        /// Send `{function.name}` call to contract
        #[ink(message)]
        pub fn {function.name | snake}({{ for input in function.inputs }}{input.name}: {input.rust_type}{{ if not @last }}, {{ endif }}{{ endfor }}) -> {function.output} \{
            let encoded_input = Self::{function.name | snake}_encode({{ for input in function.inputs }}{input.name}{{ if not @last }}, {{ endif }}{{ endfor }});
            self.env()
                .extension()
                .xvm_call(
                    super::EVM_ID,
                    Vec::from(self.evm_address.as_ref()),
                    encoded_input,
                )
                .is_ok()
        }

        fn {function.name | snake}_encode({{ for input in function.inputs }}{input.name}: {input.rust_type}{{ if not @last }}, {{ endif }}{{ endfor }}) -> Vec<u8> \{
            let mut encoded = {function.name | upper_snake}_SELECTOR.to_vec();
            let input = [
                {{ for input in function.inputs }}{input.name}.tokenize(){{ if not @last }},
                {{ endif }}{{ endfor }}
            ];

            encoded.extend(&ethabi::encode(&input));
            encoded
        }
{{ endfor }}
    }

    trait Tokenize \{
        fn tokenize(&self) -> Token;
    }

    impl<T: Tokenize> Tokenize for Vec<T> \{
        fn tokenize(&self) -> Token \{
            Token::Array(self.iter().map(Tokenize::tokenize).collect())
        }
    }

    impl<A: Tokenize, B: Tokenize> Tokenize for (A, B) \{
        fn tokenize(&self) -> Token \{
            Token::Tuple(vec![self.0.tokenize(), self.1.tokenize()])
        }
    }

    impl Tokenize for H160 \{
        fn tokenize(&self) -> Token \{
            Token::Address(*self)
        }
    }

    impl Tokenize for U256 \{
        fn tokenize(&self) -> Token \{
            Token::Uint(*self)
        }
    }

    impl Tokenize for bool \{
        fn tokenize(&self) -> Token \{
            Token::Bool(*self)
        }
    }

    impl<T: Tokenize, const N: usize> Tokenize for [T; N] \{
        fn tokenize(&self) -> Token \{
            Token::FixedArray(self.iter().map(Tokenize::tokenize).collect())
        }
    }
}
"#;

#[derive(Serialize)]
struct Input {
    name: String,

    // Type came from metadata
    evm_type: String,

    // Equivalent type to use in ink! code
    rust_type: String,
}

#[derive(Serialize)]
struct Function {
    name: String,
    inputs: Vec<Input>,
    output: String,
    selector: String,
    selector_hash: String,
}

#[derive(Serialize)]
struct Module {
    name: String,
    evm_id: String,
    functions: Vec<Function>,
}

fn convert_type(ty: &ParamType) -> String {
    match ty {
        ParamType::Bool => "bool".to_owned(),
        ParamType::Address => "H160".to_owned(),
        ParamType::Array(inner) => format!("Vec<{}>", convert_type(inner)),
        ParamType::FixedArray(inner, size) => format!("[{}; {}]", convert_type(inner), size),
        ParamType::Tuple(inner) => format!("({})", inner.iter().map(convert_type).join(", ")),
        ParamType::Uint(_size) => "U256".to_owned(), // TODO use correct size
        ParamType::FixedBytes(size) => format!("[u8; {}]", size),
        ParamType::Bytes => "Vec<u8>".to_owned(),

        _ => todo!("convert_type for {:?}", ty)
    }
}

fn main() -> Result<(), String> {
    let args = Args::parse();

    let mut reader: Box<dyn BufRead> = match args.input {
        Some(filename) => Box::new(BufReader::new(fs::File::open(filename).map_err(|e| e.to_string())?)),
        None => Box::new(BufReader::new(io::stdin())),
    };

    let mut writer: Box<dyn Write> = match args.output {
        Some(filename) => Box::new(BufWriter::new(fs::File::create(filename).map_err(|e| e.to_string())?)),
        None => Box::new(BufWriter::new(io::stdout())),
    };

    let mut buf = String::new();
    reader.read_to_string(&mut buf).map_err(|e| e.to_string())?;

    let parsed = json::parse(&buf).map_err(|e| e.to_string())?;

    let mut template = TinyTemplate::new();
    template.set_default_formatter(&format_unescaped);

    template.add_template("module", MODULE_TEMPLATE).map_err(|e| e.to_string())?;

    template.add_formatter("snake", |value, buf| match value {
        serde_json::Value::String(s) => { *buf += &s.to_case(Case::Snake); Ok(()) },
        _ => Err(tinytemplate::error::Error::GenericError { msg: "string value expected".to_owned() }),
    });

    template.add_formatter("upper_snake", |value, buf| match value {
        serde_json::Value::String(s) => { *buf += &s.to_case(Case::UpperSnake); Ok(()) },
        _ => Err(tinytemplate::error::Error::GenericError { msg: "string value expected".to_owned() }),
    });

    template.add_formatter("capitalize", |value, buf| match value {
        serde_json::Value::String(s) => {
            let (head, tail) = s.split_at(1);

            *buf += &head.to_uppercase();
            *buf += tail;

            Ok(())
        },
        _ => Err(tinytemplate::error::Error::GenericError { msg: "string value expected".to_owned() }),
    });

    template.add_formatter("convert_type", |value, buf| match value {
        serde_json::Value::String(raw_type) => {
            let param_type = ethabi::param_type::Reader::read(raw_type).unwrap();
            let converted = convert_type(&param_type);

            buf.push_str(&converted);
            Ok(())
        },

        _ => Err(tinytemplate::error::Error::GenericError { msg: "string value expected".to_owned() }),
    });

    let functions: Vec<_> = parsed
        .members()
        .filter(|item| item["type"] == "function" )
        .filter(|item| item["stateMutability"] != "view" )
        .filter(|item| item["outputs"].members().all(|output| output["type"] == "bool"))
        .map(|function| {
            let function_name = function["name"].to_string();

            let inputs: Vec<_> = function["inputs"].members().map(|m| {
                let raw_type = m["type"].as_str().unwrap();
                let param_type = ethabi::param_type::Reader::read(raw_type).unwrap();
                let converted = convert_type(&param_type);

                Input {
                    name: m["name"].to_string(),
                    evm_type: raw_type.to_string(),
                    rust_type: converted,
                }
            }).collect();

            // let outputs: String = function["outputs"].members().map(|m| format!("{}: {}, ", m["name"], m["type"])).collect();

            let selector = format!("{name}({args})",
                name = function_name,
                args = inputs.iter().map(|input| input.evm_type.as_str()).join(","),
            );

            let mut hasher = Keccak256::new();
            hasher.update(selector.as_bytes());
            let selector_hash: &[u8] = &hasher.finalize();
            let selector_hash: [u8; 4] = selector_hash[0..=3].try_into().unwrap();

            Function {
                name: function_name,
                inputs,
                output: "bool".to_owned(),
                selector,
                selector_hash: selector_hash.encode_hex(),
            }
        })
        .collect();

    let module = Module {
        name: args.module_name,
        evm_id: args.evm_id,
        functions,
    };

    let rendered = template.render("module", &module).map_err(|e| e.to_string())?;
    write!(writer, "{}\n", rendered).map_err(|e| e.to_string())?;

    Ok(())
}
