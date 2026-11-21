export interface DeviceInfo {
  name: string;
  ip: string;
  last_seen: number;
}

export interface AppSettings {
  theme: 'system' | 'light' | 'dark';
  manual_ip: string;
}

export interface FileReceivedPayload {
  id: string;
  filename: string;
  temp_path: string;
  size: number;
  source: string;
  sender_ip: string | null;
}

export interface IncomingTransferRequest {
  id: string;
  filename: string;
  size: number;
  sender_ip: string;
  sender_name: string | null;
}

export interface ServerStatus {
  http_ok: boolean;
  http_error: string | null;
  discovery_sender_ok: boolean;
  discovery_sender_error: string | null;
  discovery_receiver_ok: boolean;
  discovery_receiver_error: string | null;
}

export interface StatusResponse {
  status: string;
  local_ip: string;
}

export interface TransferProgressPayload {
  percent: number;
  sent?: number;
  total?: number;
}

export interface TransferCompletePayload {
  status: 'success' | 'error' | 'ok';
  message?: string;
}
