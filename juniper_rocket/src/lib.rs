/*!

# juniper_rocket

This repository contains the [Rocket][Rocket] web server integration for
[Juniper][Juniper], a [GraphQL][GraphQL] implementation for Rust.

## Documentation

For documentation, including guides and examples, check out [Juniper][Juniper].

A basic usage example can also be found in the [Api documentation][documentation].

## Examples

Check [examples/rocket_server.rs][example] for example code of a working Rocket
server with GraphQL handlers.

## Links

* [Juniper][Juniper]
* [Api Reference][documentation]
* [Rocket][Rocket]

## License

This project is under the BSD-2 license.

Check the LICENSE file for details.

[Rocket]: https://rocket.rs
[Juniper]: https://github.com/graphql-rust/juniper
[GraphQL]: http://graphql.org
[documentation]: https://docs.rs/juniper_rocket
[example]: https://github.com/graphql-rust/juniper_rocket/blob/master/examples/rocket_server.rs

*/

#![doc(html_root_url = "https://docs.rs/juniper_rocket/0.2.0")]
#![feature(decl_macro, proc_macro_hygiene)]

use std::{error::Error, io::Cursor};

use rocket::{
    data::{FromDataFuture, FromDataSimple},
    http::{ContentType, RawStr, Status},
    request::{FormItems, FromForm, FromFormValue},
    response::{content, Responder, Response, ResultFuture},
    Data,
    Outcome::{Failure, Forward, Success},
    Request,
};

use juniper::{http, InputValue};

use juniper::{
    serde::Deserialize, DefaultScalarValue, FieldError, GraphQLType, RootNode, ScalarRefValue,
    ScalarValue,
};

#[cfg(feature = "async")]
use juniper::GraphQLTypeAsync;

#[cfg(feature = "async")]
use futures::future::{FutureExt, TryFutureExt};

#[derive(Debug, serde_derive::Deserialize, PartialEq)]
#[serde(untagged)]
#[serde(bound = "InputValue<S>: Deserialize<'de>")]
enum GraphQLBatchRequest<S = DefaultScalarValue>
where
    S: ScalarValue + Sync + Send,
{
    Single(http::GraphQLRequest<S>),
    Batch(Vec<http::GraphQLRequest<S>>),
}

#[derive(serde_derive::Serialize)]
#[serde(untagged)]
enum GraphQLBatchResponse<'a, S = DefaultScalarValue>
where
    S: ScalarValue + Sync + Send,
{
    Single(http::GraphQLResponse<'a, S>),
    Batch(Vec<http::GraphQLResponse<'a, S>>),
}

impl<S> GraphQLBatchRequest<S>
where
    S: ScalarValue + Send + Sync,
    for<'b> &'b S: ScalarRefValue<'b>,
{
    pub fn execute<'a, CtxT, QueryT, MutationT>(
        &'a self,
        root_node: &'a RootNode<QueryT, MutationT, S>,
        context: &CtxT,
    ) -> GraphQLBatchResponse<'a, S>
    where
        QueryT: GraphQLType<S, Context = CtxT>,
        MutationT: GraphQLType<S, Context = CtxT>,
    {
        match self {
            &GraphQLBatchRequest::Single(ref request) => {
                GraphQLBatchResponse::Single(request.execute(root_node, context))
            }
            &GraphQLBatchRequest::Batch(ref requests) => GraphQLBatchResponse::Batch(
                requests
                    .iter()
                    .map(|request| request.execute(root_node, context))
                    .collect(),
            ),
        }
    }

    #[cfg(feature = "async")]
    pub async fn execute_async<'a, CtxT, QueryT, MutationT>(
        &'a self,
        root_node: &'a RootNode<'_, QueryT, MutationT, S>,
        context: &'a CtxT,
    ) -> GraphQLBatchResponse<'a, S>
    where
        QueryT: GraphQLTypeAsync<S, Context = CtxT> + Send + Sync,
        QueryT::TypeInfo: Send + Sync,
        MutationT: GraphQLTypeAsync<S, Context = CtxT> + Send + Sync,
        MutationT::TypeInfo: Send + Sync,
        CtxT: Send + Sync,
    {
        match self {
            &GraphQLBatchRequest::Single(ref request) => {
                GraphQLBatchResponse::Single(request.execute_async(root_node, context).await)
            }
            &GraphQLBatchRequest::Batch(ref requests) => {
                let futures = requests
                    .iter()
                    .map(|request| request.execute_async(root_node, context))
                    .collect::<Vec<_>>();

                GraphQLBatchResponse::Batch(futures::future::join_all(futures).await)
            }
        }
    }

    pub fn operation_names(&self) -> Vec<Option<&str>> {
        match self {
            GraphQLBatchRequest::Single(req) => vec![req.operation_name()],
            GraphQLBatchRequest::Batch(reqs) => {
                reqs.iter().map(|req| req.operation_name()).collect()
            }
        }
    }
}

impl<'a, S> GraphQLBatchResponse<'a, S>
where
    S: ScalarValue + Send + Sync,
{
    fn is_ok(&self) -> bool {
        match self {
            &GraphQLBatchResponse::Single(ref response) => response.is_ok(),
            &GraphQLBatchResponse::Batch(ref responses) => responses
                .iter()
                .fold(true, |ok, response| ok && response.is_ok()),
        }
    }
}

/// Simple wrapper around an incoming GraphQL request
///
/// See the `http` module for more information. This type can be constructed
/// automatically from both GET and POST routes by implementing the `FromForm`
/// and `FromData` traits.
#[derive(Debug, PartialEq)]
pub struct GraphQLRequest<S = DefaultScalarValue>(GraphQLBatchRequest<S>)
where
    S: ScalarValue + Send + Sync;

/// Simple wrapper around the result of executing a GraphQL query
pub struct GraphQLResponse(pub Status, pub String);

/// Generate an HTML page containing GraphiQL
pub fn graphiql_source(graphql_endpoint_url: &str) -> content::Html<String> {
    content::Html(juniper::graphiql::graphiql_source(graphql_endpoint_url))
}

/// Generate an HTML page containing GraphQL Playground
pub fn playground_source(graphql_endpoint_url: &str) -> content::Html<String> {
    content::Html(juniper::http::playground::playground_source(
        graphql_endpoint_url,
    ))
}

impl<S> GraphQLRequest<S>
where
    S: ScalarValue + Sync + Send,
    for<'b> &'b S: ScalarRefValue<'b>,
{
    /// Execute an incoming GraphQL query
    pub fn execute<CtxT, QueryT, MutationT>(
        &self,
        root_node: &RootNode<QueryT, MutationT, S>,
        context: &CtxT,
    ) -> GraphQLResponse
    where
        QueryT: GraphQLType<S, Context = CtxT>,
        MutationT: GraphQLType<S, Context = CtxT>,
    {
        let response = self.0.execute(root_node, context);
        let status = if response.is_ok() {
            Status::Ok
        } else {
            Status::BadRequest
        };
        let json = serde_json::to_string(&response).unwrap();

        GraphQLResponse(status, json)
    }

    /// Asynchronously execute an incoming GraphQL query
    #[cfg(feature = "async")]
    pub async fn execute_async<CtxT, QueryT, MutationT>(
        &self,
        root_node: &RootNode<'_, QueryT, MutationT, S>,
        context: &CtxT,
    ) -> GraphQLResponse
    where
        QueryT: GraphQLTypeAsync<S, Context = CtxT> + Send + Sync,
        QueryT::TypeInfo: Send + Sync,
        MutationT: GraphQLTypeAsync<S, Context = CtxT> + Send + Sync,
        MutationT::TypeInfo: Send + Sync,
        CtxT: Send + Sync,
    {
        let response = self.0.execute_async(root_node, context).await;
        let status = if response.is_ok() {
            Status::Ok
        } else {
            Status::BadRequest
        };
        let json = serde_json::to_string(&response).unwrap();

        GraphQLResponse(status, json)
    }

    /// Returns the operation names associated with this request.
    ///
    /// For batch requests there will be multiple names.
    pub fn operation_names(&self) -> Vec<Option<&str>> {
        self.0.operation_names()
    }
}

impl GraphQLResponse {
    /// Constructs an error response outside of the normal execution flow
    ///
    /// # Examples
    ///
    /// ```
    /// # #![feature(decl_macro, proc_macro_hygiene)]
    /// #
    /// # extern crate juniper;
    /// # extern crate juniper_rocket;
    /// # extern crate rocket;
    /// #
    /// # use rocket::http::Cookies;
    /// # use rocket::request::Form;
    /// # use rocket::response::content;
    /// # use rocket::State;
    /// #
    /// # use juniper::tests::schema::Query;
    /// # use juniper::tests::model::Database;
    /// # use juniper::{EmptyMutation, FieldError, RootNode, Value};
    /// #
    /// # type Schema = RootNode<'static, Query, EmptyMutation<Database>>;
    /// #
    /// #[rocket::get("/graphql?<request..>")]
    /// fn get_graphql_handler(
    ///     mut cookies: Cookies,
    ///     context: State<Database>,
    ///     request: Form<juniper_rocket::GraphQLRequest>,
    ///     schema: State<Schema>,
    /// ) -> juniper_rocket::GraphQLResponse {
    ///     if cookies.get_private("user_id").is_none() {
    ///         let err = FieldError::new("User is not logged in", Value::null());
    ///         return juniper_rocket::GraphQLResponse::error(err);
    ///     }
    ///
    ///     request.execute(&schema, &context)
    /// }
    /// ```
    pub fn error(error: FieldError) -> Self {
        let response = http::GraphQLResponse::error(error);
        let json = serde_json::to_string(&response).unwrap();
        GraphQLResponse(Status::BadRequest, json)
    }

    /// Constructs a custom response outside of the normal execution flow
    ///
    /// This is intended for highly customized integrations and should only
    /// be used as a last resort. For normal juniper use, use the response
    /// from GraphQLRequest::execute(..).
    pub fn custom(status: Status, response: serde_json::Value) -> Self {
        let json = serde_json::to_string(&response).unwrap();
        GraphQLResponse(status, json)
    }
}

impl<'f, S> FromForm<'f> for GraphQLRequest<S>
where
    S: ScalarValue + Send + Sync,
{
    type Error = String;

    fn from_form(form_items: &mut FormItems<'f>, strict: bool) -> Result<Self, String> {
        let mut query = None;
        let mut operation_name = None;
        let mut variables = None;

        for form_item in form_items {
            let (key, value) = form_item.key_value();
            // Note: we explicitly decode in the match arms to save work rather
            // than decoding every form item blindly.
            match key.as_str() {
                "query" => {
                    if query.is_some() {
                        return Err("Query parameter must not occur more than once".to_owned());
                    } else {
                        match value.url_decode() {
                            Ok(v) => query = Some(v),
                            Err(e) => return Err(e.description().to_string()),
                        }
                    }
                }
                "operation_name" => {
                    if operation_name.is_some() {
                        return Err(
                            "Operation name parameter must not occur more than once".to_owned()
                        );
                    } else {
                        match value.url_decode() {
                            Ok(v) => operation_name = Some(v),
                            Err(e) => return Err(e.description().to_string()),
                        }
                    }
                }
                "variables" => {
                    if variables.is_some() {
                        return Err("Variables parameter must not occur more than once".to_owned());
                    } else {
                        let decoded;
                        match value.url_decode() {
                            Ok(v) => decoded = v,
                            Err(e) => return Err(e.description().to_string()),
                        }
                        variables = Some(
                            serde_json::from_str::<InputValue<_>>(&decoded)
                                .map_err(|err| err.description().to_owned())?,
                        );
                    }
                }
                _ => {
                    if strict {
                        return Err(format!("Prohibited extra field '{}'", key).to_owned());
                    }
                }
            }
        }

        if let Some(query) = query {
            Ok(GraphQLRequest(GraphQLBatchRequest::Single(
                http::GraphQLRequest::new(query, operation_name, variables),
            )))
        } else {
            Err("Query parameter missing".to_owned())
        }
    }
}

impl<'v, S> FromFormValue<'v> for GraphQLRequest<S>
where
    S: ScalarValue + Send + Sync,
{
    type Error = String;

    fn from_form_value(form_value: &'v RawStr) -> Result<Self, Self::Error> {
        let mut form_items = FormItems::from(form_value);

        Self::from_form(&mut form_items, true)
    }
}

const BODY_LIMIT: u64 = 1024 * 100;

impl<S> FromDataSimple for GraphQLRequest<S>
where
    S: ScalarValue + Send + Sync,
{
    type Error = String;

    fn from_data(request: &Request, data: Data) -> FromDataFuture<'static, Self, Self::Error> {
        use tokio::io::AsyncReadExt as _;

        if !request.content_type().map_or(false, |ct| ct.is_json()) {
            return Box::pin(async move { Forward(data) });
        }

        Box::pin(async move {
            let mut body = String::new();
            let mut reader = data.open().take(BODY_LIMIT);
            if let Err(e) = reader.read_to_string(&mut body).await {
                return Failure((Status::InternalServerError, format!("{:?}", e)));
            }

            match serde_json::from_str(&body) {
                Ok(value) => Success(GraphQLRequest(value)),
                Err(failure) => Failure((Status::BadRequest, format!("{}", failure))),
            }
        })
    }
}

impl<'r> Responder<'r> for GraphQLResponse {
    fn respond_to(self, _: &Request) -> ResultFuture<'r> {
        let GraphQLResponse(status, body) = self;

        Box::pin(async move {
            Ok(Response::build()
                .header(ContentType::new("application", "json"))
                .status(status)
                .sized_body(Cursor::new(body))
                .finalize())
        })
    }
}

#[cfg(test)]
mod fromform_tests {
    use super::*;
    use juniper::InputValue;
    use rocket::request::{FormItems, FromForm};
    use std::str;

    fn check_error(input: &str, error: &str, strict: bool) {
        let mut items = FormItems::from(input);
        let result: Result<GraphQLRequest, _> = GraphQLRequest::from_form(&mut items, strict);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error);
    }

    #[test]
    fn test_empty_form() {
        check_error("", "Query parameter missing", false);
    }

    #[test]
    fn test_no_query() {
        check_error(
            "operation_name=foo&variables={}",
            "Query parameter missing",
            false,
        );
    }

    #[test]
    fn test_strict() {
        check_error("query=test&foo=bar", "Prohibited extra field \'foo\'", true);
    }

    #[test]
    fn test_duplicate_query() {
        check_error(
            "query=foo&query=bar",
            "Query parameter must not occur more than once",
            false,
        );
    }

    #[test]
    fn test_duplicate_operation_name() {
        check_error(
            "query=test&operation_name=op1&operation_name=op2",
            "Operation name parameter must not occur more than once",
            false,
        );
    }

    #[test]
    fn test_duplicate_variables() {
        check_error(
            "query=test&variables={}&variables={}",
            "Variables parameter must not occur more than once",
            false,
        );
    }

    #[test]
    fn test_variables_invalid_json() {
        check_error("query=test&variables=NOT_JSON", "JSON error", false);
    }

    #[test]
    fn test_variables_valid_json() {
        let form_string = r#"query=test&variables={"foo":"bar"}"#;
        let mut items = FormItems::from(form_string);
        let result = GraphQLRequest::from_form(&mut items, false);
        assert!(result.is_ok());
        let variables = ::serde_json::from_str::<InputValue>(r#"{"foo":"bar"}"#).unwrap();
        let expected = GraphQLRequest(GraphQLBatchRequest::Single(http::GraphQLRequest::new(
            "test".to_string(),
            None,
            Some(variables),
        )));
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_variables_encoded_json() {
        let form_string = r#"query=test&variables={"foo": "x%20y%26%3F+z"}"#;
        let mut items = FormItems::from(form_string);
        let result = GraphQLRequest::from_form(&mut items, false);
        assert!(result.is_ok());
        let variables = ::serde_json::from_str::<InputValue>(r#"{"foo":"x y&? z"}"#).unwrap();
        let expected = GraphQLRequest(GraphQLBatchRequest::Single(http::GraphQLRequest::new(
            "test".to_string(),
            None,
            Some(variables),
        )));
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_url_decode() {
        let form_string = "query=%25foo%20bar+baz%26%3F&operation_name=test";
        let mut items = FormItems::from(form_string);
        let result: Result<GraphQLRequest, _> = GraphQLRequest::from_form(&mut items, false);
        assert!(result.is_ok());
        let expected = GraphQLRequest(GraphQLBatchRequest::Single(http::GraphQLRequest::new(
            "%foo bar baz&?".to_string(),
            Some("test".to_string()),
            None,
        )));
        assert_eq!(result.unwrap(), expected);
    }
}

#[cfg(test)]
mod tests {

    use rocket::{
        self, get,
        http::ContentType,
        local::{Client, LocalRequest},
        post,
        request::Form,
        routes, Rocket, State,
    };

    use juniper::{
        http::tests as http_tests,
        tests::{model::Database, schema::Query},
        EmptyMutation, RootNode,
    };

    type Schema = RootNode<'static, Query, EmptyMutation<Database>>;

    #[get("/?<request..>")]
    fn get_graphql_handler(
        context: State<Database>,
        request: Form<super::GraphQLRequest>,
        schema: State<Schema>,
    ) -> super::GraphQLResponse {
        request.execute(&schema, &context)
    }

    #[post("/", data = "<request>")]
    fn post_graphql_handler(
        context: State<Database>,
        request: super::GraphQLRequest,
        schema: State<Schema>,
    ) -> super::GraphQLResponse {
        request.execute(&schema, &context)
    }

    struct TestRocketIntegration {
        client: Client,
    }

    impl http_tests::HTTPIntegration for TestRocketIntegration {
        fn get(&self, url: &str) -> http_tests::TestResponse {
            let req = &self.client.get(url);
            make_test_response(req)
        }

        fn post(&self, url: &str, body: &str) -> http_tests::TestResponse {
            let req = &self.client.post(url).header(ContentType::JSON).body(body);
            make_test_response(req)
        }
    }

    #[test]
    fn test_rocket_integration() {
        let rocket = make_rocket();
        let client = Client::new(rocket).expect("valid rocket");
        let integration = TestRocketIntegration { client };

        http_tests::run_http_test_suite(&integration);
    }

    #[test]
    fn test_operation_names() {
        #[post("/", data = "<request>")]
        fn post_graphql_assert_operation_name_handler(
            context: State<Database>,
            request: super::GraphQLRequest,
            schema: State<Schema>,
        ) -> super::GraphQLResponse {
            assert_eq!(request.operation_names(), vec![Some("TestQuery")]);
            request.execute(&schema, &context)
        }

        let rocket = make_rocket_without_routes()
            .mount("/", routes![post_graphql_assert_operation_name_handler]);
        let client = Client::new(rocket).expect("valid rocket");

        let req = client
            .post("/")
            .header(ContentType::JSON)
            .body(r#"{"query": "query TestQuery {hero{name}}", "operationName": "TestQuery"}"#);
        let resp = make_test_response(&req);

        assert_eq!(resp.status_code, 200);
    }

    fn make_rocket() -> Rocket {
        make_rocket_without_routes().mount("/", routes![post_graphql_handler, get_graphql_handler])
    }

    fn make_rocket_without_routes() -> Rocket {
        rocket::ignite()
            .manage(Database::new())
            .manage(Schema::new(Query, EmptyMutation::<Database>::new()))
    }

    fn make_test_response(request: &LocalRequest) -> http_tests::TestResponse {
        let mut response = request.clone().dispatch();
        let status_code = response.status().code as i32;
        let content_type = response
            .content_type()
            .expect("No content type header from handler")
            .to_string();
        let body = response
            .body()
            .expect("No body returned from GraphQL handler")
            .into_string();

        http_tests::TestResponse {
            status_code,
            body,
            content_type,
        }
    }
}
