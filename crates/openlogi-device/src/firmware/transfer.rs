//! Sans-I/O RDFU acknowledgement and resume validation.

use std::time::Duration;

use hidpp::feature::dfu_resumable::{DFU_PACKET_LEN, DfuStatus, DfuStatusReport};
use openlogi_core::hid::FirmwareUpdateFailure;

use super::FirmwareImage;

pub(super) enum TransferAction {
    Send([u8; DFU_PACKET_LEN]),
    Wait(Duration),
    Complete,
}

pub(super) struct Transfer<'a> {
    image: &'a FirmwareImage,
    sent: Option<(usize, usize)>,
    high_water: usize,
}

fn protocol(message: &str) -> FirmwareUpdateFailure {
    FirmwareUpdateFailure::Protocol(message.into())
}

impl<'a> Transfer<'a> {
    pub fn new(image: &'a FirmwareImage) -> Self {
        Self {
            image,
            sent: None,
            high_water: 0,
        }
    }

    fn packets(&self, block: usize) -> usize {
        self.image.blocks[block].len() / DFU_PACKET_LEN
    }

    fn acknowledged(&mut self, block: usize, packet: usize) {
        let count = self.image.blocks[..block]
            .iter()
            .map(|data| data.len() / DFU_PACKET_LEN)
            .sum::<usize>()
            + packet;
        self.high_water = self.high_water.max(count);
    }

    pub fn percent(&self) -> u8 {
        let total = self
            .image
            .blocks
            .iter()
            .map(|data| data.len() / DFU_PACKET_LEN)
            .sum::<usize>();
        u8::try_from(self.high_water * 100 / total).unwrap_or(100)
    }

    pub fn accept(
        &mut self,
        report: DfuStatusReport,
        recovering: bool,
    ) -> Result<TransferAction, FirmwareUpdateFailure> {
        if report.entity != self.image.entity {
            return Err(protocol("status names a different firmware bank"));
        }
        // Every packet must be acknowledged in this run, so the first status
        // must ask for block zero. Anything further along is an earlier run's
        // leftover, whose bytes nothing proves came from this image.
        if self.sent.is_none()
            && matches!(
                report.status,
                DfuStatus::TransferReady { block: 1.. }
                    | DfuStatus::PacketAck { .. }
                    | DfuStatus::TransferComplete
                    | DfuStatus::ApplyPending
            )
        {
            return Err(protocol(
                "device kept an earlier transfer instead of restarting from the first block",
            ));
        }
        let (block, packet) = match report.status {
            DfuStatus::TransferReady { block } => {
                let block = usize::from(block);
                if block >= self.image.blocks.len() {
                    return Err(protocol("device requested an absent block"));
                }
                match self.sent {
                    None if block == 0 => {}
                    Some((previous, packet))
                        if block == previous + 1 && packet + 1 == self.packets(previous) => {}
                    Some((previous, _)) if recovering && block == previous => {}
                    _ => return Err(protocol("device skipped or rewound a committed block")),
                }
                self.acknowledged(block, 0);
                (block, 0)
            }
            DfuStatus::PacketAck { next_packet } => {
                let (block, sent) = self
                    .sent
                    .ok_or_else(|| protocol("packet acknowledgement before first packet"))?;
                let next = usize::try_from(next_packet)
                    .map_err(|_| protocol("oversized packet acknowledgement"))?;
                // A polled status may still request the packet whose native
                // write failed. Only recovery can resend that exact position;
                // ordinary replies must advance and older positions stay invalid.
                let retry = recovering && next == sent;
                if (!retry && next != sent + 1) || next >= self.packets(block) {
                    return Err(protocol(
                        "non-monotonic packet acknowledgement or missing block transition",
                    ));
                }
                self.acknowledged(block, next);
                (block, next)
            }
            DfuStatus::TransferWait { delay_ms } => {
                if self.sent.is_none() {
                    return Err(protocol("wait before first transfer block"));
                }
                return Ok(TransferAction::Wait(Duration::from_millis(
                    (u64::from(delay_ms) * 3).clamp(150, 8000),
                )));
            }
            DfuStatus::TransferComplete => {
                let (block, packet) = self
                    .sent
                    .ok_or_else(|| protocol("completion before first packet"))?;
                if block + 1 != self.image.blocks.len() || packet + 1 != self.packets(block) {
                    return Err(protocol("completion before all image bytes were sent"));
                }
                self.acknowledged(block, packet + 1);
                return Ok(TransferAction::Complete);
            }
            DfuStatus::Error(error) => {
                return Err(FirmwareUpdateFailure::DeviceRefused { code: error as u8 });
            }
            DfuStatus::NotStarted | DfuStatus::ApplyPending | DfuStatus::Aborted => {
                return Err(protocol("unexpected firmware update state"));
            }
        };
        let mut bytes = [0; DFU_PACKET_LEN];
        bytes.copy_from_slice(
            &self.image.blocks[block][packet * DFU_PACKET_LEN..(packet + 1) * DFU_PACKET_LEN],
        );
        self.sent = Some((block, packet));
        Ok(TransferAction::Send(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::hid::FirmwareVersion;

    fn image() -> FirmwareImage {
        FirmwareImage {
            entity: 1,
            product_id: 0xb042,
            prefix: "RBM".into(),
            version: FirmwareVersion {
                number: 27,
                revision: 3,
                build: 19,
            },
            magic: *b"RBM27_D1\0\0",
            blocks: vec![vec![0x11; 32], vec![0x22; 16]],
        }
    }

    fn report(status: DfuStatus) -> DfuStatusReport {
        DfuStatusReport { entity: 1, status }
    }

    #[test]
    fn follows_ack_wait_and_block_commit_before_completion() {
        let image = image();
        let mut transfer = Transfer::new(&image);
        assert!(
            matches!(transfer.accept(report(DfuStatus::TransferReady { block: 0 }), false).unwrap(), TransferAction::Send(bytes) if bytes == [0x11; 16])
        );
        assert!(
            matches!(transfer.accept(report(DfuStatus::PacketAck { next_packet: 1 }), false).unwrap(), TransferAction::Send(bytes) if bytes == [0x11; 16])
        );
        assert_eq!(transfer.percent(), 33);
        assert!(matches!(
            transfer
                .accept(report(DfuStatus::TransferWait { delay_ms: 10 }), false)
                .unwrap(),
            TransferAction::Wait(_)
        ));
        assert!(
            matches!(transfer.accept(report(DfuStatus::TransferReady { block: 1 }), false).unwrap(), TransferAction::Send(bytes) if bytes == [0x22; 16])
        );
        assert!(matches!(
            transfer
                .accept(report(DfuStatus::TransferComplete), false)
                .unwrap(),
            TransferAction::Complete
        ));
        assert_eq!(transfer.percent(), 100);
    }

    #[test]
    fn a_recovery_can_restart_only_the_uncommitted_block() {
        let image = image();
        let mut transfer = Transfer::new(&image);
        transfer
            .accept(report(DfuStatus::TransferReady { block: 0 }), false)
            .unwrap();
        transfer
            .accept(report(DfuStatus::PacketAck { next_packet: 1 }), false)
            .unwrap();
        transfer
            .accept(report(DfuStatus::TransferReady { block: 0 }), true)
            .unwrap();
        assert_eq!(transfer.percent(), 33, "published progress never rewinds");
        transfer
            .accept(report(DfuStatus::PacketAck { next_packet: 1 }), false)
            .unwrap();
        transfer
            .accept(report(DfuStatus::TransferReady { block: 1 }), false)
            .unwrap();
        assert!(matches!(
            transfer.accept(report(DfuStatus::TransferReady { block: 0 }), true),
            Err(FirmwareUpdateFailure::Protocol(_))
        ));
    }

    #[test]
    fn recovery_can_retry_only_the_last_attempted_packet() {
        let image = image();
        let mut transfer = Transfer::new(&image);
        transfer
            .accept(report(DfuStatus::TransferReady { block: 0 }), false)
            .unwrap();
        transfer
            .accept(report(DfuStatus::PacketAck { next_packet: 1 }), false)
            .unwrap();
        assert!(matches!(
            transfer.accept(report(DfuStatus::PacketAck { next_packet: 1 }), false),
            Err(FirmwareUpdateFailure::Protocol(_))
        ));
        for next_packet in [0, 2, 3, u32::MAX] {
            assert!(matches!(
                transfer.accept(report(DfuStatus::PacketAck { next_packet }), true),
                Err(FirmwareUpdateFailure::Protocol(_))
            ));
        }
        assert!(matches!(
            transfer
                .accept(report(DfuStatus::PacketAck { next_packet: 1 }), true)
                .unwrap(),
            TransferAction::Send(bytes) if bytes == [0x11; DFU_PACKET_LEN]
        ));
        assert_eq!(transfer.percent(), 33, "a retry does not advance progress");
        transfer
            .accept(report(DfuStatus::TransferReady { block: 1 }), false)
            .unwrap();
        assert!(matches!(
            transfer
                .accept(report(DfuStatus::TransferComplete), false)
                .unwrap(),
            TransferAction::Complete
        ));
    }

    #[test]
    fn an_earlier_runs_leftover_position_is_never_resumed() {
        let image = image();
        for status in [
            DfuStatus::PacketAck { next_packet: 1 },
            DfuStatus::TransferReady { block: 1 },
            DfuStatus::TransferComplete,
            DfuStatus::ApplyPending,
        ] {
            for recovering in [false, true] {
                let mut transfer = Transfer::new(&image);
                assert!(
                    matches!(
                        transfer.accept(report(status), recovering),
                        Err(FirmwareUpdateFailure::Protocol(message))
                            if message.contains("earlier transfer")
                    ),
                    "{status:?} must not resume"
                );
                assert_eq!(transfer.percent(), 0);
            }
        }
    }

    #[test]
    fn rejects_premature_completion_skips_duplicate_acks_and_wrong_entity() {
        let image = image();
        for status in [
            DfuStatus::TransferComplete,
            DfuStatus::TransferReady { block: 1 },
            DfuStatus::PacketAck { next_packet: 0 },
            DfuStatus::TransferWait { delay_ms: 1 },
        ] {
            let mut transfer = Transfer::new(&image);
            assert!(matches!(
                transfer.accept(report(status), false),
                Err(FirmwareUpdateFailure::Protocol(_))
            ));
        }
        let mut transfer = Transfer::new(&image);
        transfer
            .accept(report(DfuStatus::TransferReady { block: 0 }), false)
            .unwrap();
        for status in [
            DfuStatus::TransferComplete,
            DfuStatus::TransferReady { block: 1 },
            DfuStatus::PacketAck { next_packet: 0 },
            DfuStatus::PacketAck { next_packet: 2 },
        ] {
            assert!(matches!(
                transfer.accept(report(status), false),
                Err(FirmwareUpdateFailure::Protocol(_))
            ));
        }
        assert!(matches!(
            transfer.accept(
                DfuStatusReport {
                    entity: 2,
                    status: DfuStatus::TransferReady { block: 0 }
                },
                false
            ),
            Err(FirmwareUpdateFailure::Protocol(_))
        ));
    }
}
