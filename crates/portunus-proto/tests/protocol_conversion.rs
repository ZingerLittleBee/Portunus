use portunus_core::Protocol as CoreProto;
use portunus_proto::v1::Protocol as WireProto;

#[test]
fn core_to_wire_round_trips() {
    let cases = [
        (CoreProto::Tcp, WireProto::Tcp),
        (CoreProto::Udp, WireProto::Udp),
    ];
    for (c, w) in cases {
        assert_eq!(WireProto::from(c), w);
        assert_eq!(CoreProto::try_from(w).unwrap(), c);
    }
}

#[test]
fn wire_unspecified_fails_try_from() {
    assert!(CoreProto::try_from(WireProto::Unspecified).is_err());
}
